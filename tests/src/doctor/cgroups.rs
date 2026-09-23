use std::collections::HashSet;
use std::fs;
use std::path::Path;

use super::*;

fn create_cgroup(path: &Path, processes: &str) {
    fs::create_dir_all(path).expect("create cgroup fixture");
    fs::write(path.join("cgroup.procs"), processes).expect("write process list");
}

fn ownership(root: &Path) -> CgroupOwnership {
    CgroupOwnership {
        sandbox_paths: HashSet::from([root.join("enclave-sb-test")]),
        workspace_paths: Default::default(),
    }
}

#[test]
fn doctor_finds_nested_empty_workspace_cgroups_and_ignores_foreign_groups() {
    let root = std::env::temp_dir().join(format!(
        "enclave-doctor-cgroups-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let stale = root.join("enclave-sb-test/enclave-ws-101");
    let active = root.join("enclave-sb-test/enclave-ws-102");
    let foreign = root.join("foreign/enclave-ws-103");
    create_cgroup(&stale, "\n");
    create_cgroup(&active, "4321\n");
    create_cgroup(&foreign, "\n");

    let found = empty_workspace_cgroups(&root).expect("inventory cgroups");
    assert_eq!(found, vec![stale.clone()]);

    fs::remove_dir_all(root).expect("remove cgroup fixture");
}

#[test]
fn doctor_repair_removes_empty_nested_workspace_cgroups_but_keeps_active_ones() {
    let root = std::env::temp_dir().join(format!(
        "enclave-doctor-cgroup-repair-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let empty = root.join("enclave-sb-test/enclave-ws-101");
    let active = root.join("enclave-sb-test/enclave-ws-102");
    create_cgroup(&empty, "\n");
    create_cgroup(&active, "4321\n");

    let removed =
        remove_empty_workspace_cgroups_using(&root, &ownership(&root), remove_fixture_cgroup)
            .expect("repair empty cgroup");
    assert_eq!(removed, 1);
    assert!(!empty.exists());
    assert!(active.exists());

    fs::remove_dir_all(root).expect("remove cgroup fixture");
}

#[test]
fn doctor_repair_removes_nested_empty_groups_before_parent_workspace_group() {
    let root = std::env::temp_dir().join(format!(
        "enclave-doctor-cgroup-nested-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let parent = root.join("enclave-sb-test/enclave-ws-101");
    let child = parent.join("enclave-ws-102");
    create_cgroup(&parent, "\n");
    create_cgroup(&child, "\n");

    let removed =
        remove_empty_workspace_cgroups_using(&root, &ownership(&root), remove_fixture_cgroup)
            .expect("repair nested cgroups");
    assert_eq!(removed, 2);
    assert!(!parent.exists());

    fs::remove_dir_all(root).expect("remove cgroup fixture");
}

#[test]
fn doctor_repair_preserves_unowned_or_registry_referenced_groups() {
    let root = std::env::temp_dir().join(format!(
        "enclave-doctor-cgroup-ownership-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let referenced = root.join("enclave-sb-test/enclave-ws-101");
    let unowned = root.join("enclave-sb-other/enclave-ws-102");
    create_cgroup(&referenced, "\n");
    create_cgroup(&unowned, "\n");
    let mut owners = ownership(&root);
    owners
        .workspace_paths
        .insert(referenced.clone(), "sb/ws".to_string());

    let removed = remove_empty_workspace_cgroups_using(&root, &owners, remove_fixture_cgroup)
        .expect("repair owned cgroups");
    assert_eq!(removed, 0);
    assert!(referenced.exists());
    assert!(unowned.exists());

    fs::remove_dir_all(root).expect("remove cgroup fixture");
}

fn remove_fixture_cgroup(path: &Path) -> std::io::Result<()> {
    fs::remove_file(path.join("cgroup.procs"))?;
    fs::remove_dir(path)
}
