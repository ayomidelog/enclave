use std::fs;
use std::path::PathBuf;

use enclave::registry::{ensure_registry, repair_registry, with_registry, with_registry_mut};

fn state_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create state dir");
    dir
}

#[test]
fn atomic_registry_write_persists_changes() {
    let state = state_dir("enclave-registry-atomic");
    ensure_registry(&state).expect("registry init");

    with_registry_mut(&state, |registry| {
        registry.version = 42;
        Ok(())
    })
    .expect("registry write should succeed");

    let version = with_registry(&state, |registry| Ok(registry.version)).expect("registry read");
    assert_eq!(version, 42);
    let _ = fs::remove_dir_all(state);
}

#[test]
fn repair_recovers_from_corrupted_registry_file() {
    let state = state_dir("enclave-registry-repair");
    ensure_registry(&state).expect("registry init");

    let sandboxes = state.join("sandboxes").join("sb1");
    let workspaces = sandboxes.join("workspaces").join("ws1");
    fs::create_dir_all(&workspaces).expect("create workspace dirs");

    fs::write(
        sandboxes.join("sandbox.json"),
        r#"{
  "id": "sb1",
  "name": "sb1",
  "suite": "bookworm",
  "mirror": "http://deb.debian.org/debian",
  "created_at": "2026-01-01T00:00:00Z",
  "sandbox_path": "",
  "rootfs_path": "/tmp/rootfs",
  "mounted_rootfs_path": "",
  "workspaces_path": "",
  "home_base_path": "",
  "status": "stopped"
}"#,
    )
    .expect("write sandbox metadata");

    fs::write(
        workspaces.join("workspace.json"),
        r#"{
  "id": "ws1",
  "sandbox_id": "sb1",
  "name": "ws1",
  "created_at": "2026-01-01T00:00:00Z",
  "workspace_path": "",
  "filesystem_path": "/tmp/fs",
  "filesystem_mount_target": "/home",
  "sandbox_rootfs_path": "/tmp/rootfs",
  "overlay_home_base_path": "/tmp/home-base",
  "overlay_home_upper_path": "/tmp/home-upper",
  "overlay_home_work_path": "/tmp/home-work",
  "overlay_home_merged_path": "/tmp/home-merged",
  "status": "stopped"
}"#,
    )
    .expect("write workspace metadata");

    fs::write(state.join("registry.json"), "{ bad json").expect("corrupt registry");

    let report = repair_registry(&state, false).expect("repair should recover from corruption");
    assert!(report.added_sandboxes >= 1);

    let sandbox_count = with_registry(&state, |registry| Ok(registry.sandboxes.len()))
        .expect("registry should be readable after repair");
    assert_eq!(sandbox_count, 1);

    let _ = fs::remove_dir_all(state);
}

#[test]
fn partial_write_is_replaced_by_subsequent_atomic_update() {
    let state = state_dir("enclave-registry-partial");
    ensure_registry(&state).expect("registry init");

    fs::write(state.join("registry.json"), "{\"version\":").expect("write partial content");

    with_registry_mut(&state, |registry| {
        registry.version = 1;
        Ok(())
    })
    .expect("atomic write should repair partial registry contents");

    let version = with_registry(&state, |registry| Ok(registry.version)).expect("registry read");
    assert_eq!(version, 1);
    let _ = fs::remove_dir_all(state);
}
