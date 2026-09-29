use super::*;

use crate::registry::{with_registry_mut, RegistrySandbox};
use crate::sandbox::{BootstrapMethod, SandboxLimits, SandboxMetadata, SandboxStatus};

fn state_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "enclave-doctor-orphans-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create state dir");
    dir
}

/// A registered sandbox with no workspaces, whose workspaces directory exists.
fn register_sandbox(state: &Path, sandbox_id: &str) -> PathBuf {
    let sandbox_dir = state.join("sandboxes").join(sandbox_id);
    let workspaces = sandbox_dir.join("workspaces");
    std::fs::create_dir_all(&workspaces).expect("create workspaces dir");
    let metadata = SandboxMetadata {
        id: sandbox_id.to_string(),
        name: sandbox_id.to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: BootstrapMethod::CachedRootfs,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        sandbox_path: sandbox_dir.to_string_lossy().to_string(),
        rootfs_path: sandbox_dir.join("rootfs").to_string_lossy().to_string(),
        rootfs_lower_path: None,
        mounted_rootfs_path: sandbox_dir
            .join("runtime/rootfs.mnt")
            .to_string_lossy()
            .to_string(),
        workspaces_path: workspaces.to_string_lossy().to_string(),
        home_base_path: sandbox_dir.join("home-base").to_string_lossy().to_string(),
        limits: SandboxLimits::default(),
        status: SandboxStatus::Stopped,
    };
    with_registry_mut(state, |registry| {
        registry.sandboxes.insert(
            sandbox_id.to_string(),
            RegistrySandbox {
                metadata,
                workspaces: Default::default(),
            },
        );
        Ok(())
    })
    .expect("register sandbox");
    workspaces
}

#[test]
fn an_empty_state_directory_reports_no_orphan_runtimes() {
    let dir = state_dir("empty");
    let check = check_orphan_runtimes(&dir);
    assert_eq!(check.status, "ok", "{}", check.detail);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_workspace_directory_with_no_live_runtime_is_not_an_orphan() {
    // The registry has no record and the directory exists, but the marker says the
    // runtime is gone, so this is a leftover for repair rather than a live orphan.
    let dir = state_dir("leftover");
    let workspaces = register_sandbox(&dir, "sb-1");
    let workspace = workspaces.join("ws-1");
    std::fs::create_dir_all(workspace.join("ns")).expect("create workspace dir");
    std::fs::write(workspace.join("ns/pid.ref"), "unassigned\n").expect("write marker");

    let check = check_orphan_runtimes(&dir);
    assert_eq!(check.status, "ok", "{}", check.detail);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_workspace_directory_with_a_live_runtime_is_reported() {
    // This is the state repair leaves behind when it refuses to delete a running
    // workspace's files: a directory the registry cannot describe and a runtime
    // that is still alive. Doctor has to name it, or the leak is invisible.
    let dir = state_dir("live");
    let workspaces = register_sandbox(&dir, "sb-1");
    let workspace = workspaces.join("ws-1");
    std::fs::create_dir_all(workspace.join("ns")).expect("create workspace dir");
    let namespace = std::fs::read_link("/proc/self/ns/pid").expect("read own pid namespace");
    std::fs::write(
        workspace.join("ns/pid.ref"),
        format!("{}\n", namespace.to_string_lossy()),
    )
    .expect("write marker");

    let check = check_orphan_runtimes(&dir);
    assert_eq!(check.status, "warn", "{}", check.detail);
    assert!(check.detail.contains("sb-1/ws-1"), "{}", check.detail);
    assert!(
        check.detail.contains(&workspace.display().to_string()),
        "{}",
        check.detail
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_workspace_the_registry_knows_about_is_left_to_the_other_checks() {
    // A live marker on a workspace the registry does describe is not this check's
    // business: the registry owns that record and the lifecycle checks answer for
    // it. Reporting it here would double-report every running workspace.
    let dir = state_dir("known");
    let workspaces = register_sandbox(&dir, "sb-1");
    let workspace = workspaces.join("ws-1");
    std::fs::create_dir_all(workspace.join("ns")).expect("create workspace dir");
    let namespace = std::fs::read_link("/proc/self/ns/pid").expect("read own pid namespace");
    std::fs::write(
        workspace.join("ns/pid.ref"),
        format!("{}\n", namespace.to_string_lossy()),
    )
    .expect("write marker");

    let mut metadata = crate::workspace::WorkspaceMetadata {
        id: "ws-1".to_string(),
        sandbox_id: "sb-1".to_string(),
        name: "ws-1".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        workspace_path: workspace.to_string_lossy().to_string(),
        filesystem_path: workspace.join("fs").to_string_lossy().to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: dir
            .join("sandboxes/sb-1/rootfs")
            .to_string_lossy()
            .to_string(),
        overlay_home_base_path: String::new(),
        overlay_home_upper_path: String::new(),
        overlay_home_work_path: String::new(),
        overlay_home_merged_path: String::new(),
        auth_providers: Vec::new(),
        owner: None,
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: crate::workspace::WorkspaceStatus::Running,
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: Default::default(),
        clear_tmp_on_restart: false,
        limits: Default::default(),
        assigned_ip: None,
    };
    metadata.status = crate::workspace::WorkspaceStatus::Running;
    with_registry_mut(&dir, |registry| {
        registry
            .sandboxes
            .get_mut("sb-1")
            .expect("sandbox")
            .workspaces
            .insert("ws-1".to_string(), metadata);
        Ok(())
    })
    .expect("insert workspace");

    let check = check_orphan_runtimes(&dir);
    assert_eq!(check.status, "ok", "{}", check.detail);
    let _ = std::fs::remove_dir_all(&dir);
}
