use std::fs;
use std::path::PathBuf;

use enclave::registry::{
    ensure_registry, repair_registry, with_registry, with_registry_mut, RegistrySandbox,
};
use enclave::sandbox::{BootstrapMethod, SandboxLimits, SandboxMetadata, SandboxStatus};

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
    fs::create_dir_all(sandboxes.join("rootfs")).expect("create sandbox rootfs");
    let rootfs_path = sandboxes.join("rootfs").to_string_lossy().to_string();

    fs::write(
        sandboxes.join("sandbox.json"),
        format!(
            r#"{{
  "id": "sb1",
  "name": "sb1",
  "suite": "bookworm",
  "mirror": "http://deb.debian.org/debian",
  "created_at": "2026-01-01T00:00:00Z",
  "sandbox_path": "",
  "rootfs_path": "{}",
  "mounted_rootfs_path": "",
  "workspaces_path": "",
  "home_base_path": "",
  "status": "stopped"
}}"#,
            rootfs_path
        ),
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
fn repair_removes_stale_records_and_metadata_less_orphans() {
    let state = state_dir("enclave-registry-orphans");
    ensure_registry(&state).expect("registry init");

    let stale_dir = state.join("sandboxes").join("stale");
    fs::create_dir_all(&stale_dir).expect("create stale sandbox dir");
    let orphan_dir = state.join("sandboxes").join("orphan");
    fs::create_dir_all(&orphan_dir).expect("create metadata-less sandbox dir");

    let metadata = SandboxMetadata {
        id: "stale".to_string(),
        name: "stale".to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: BootstrapMethod::CachedRootfs,
        created_at: "2026-08-06T00:00:00Z".to_string(),
        sandbox_path: stale_dir.to_string_lossy().to_string(),
        rootfs_path: stale_dir.join("rootfs").to_string_lossy().to_string(),
        mounted_rootfs_path: stale_dir
            .join("runtime")
            .join("rootfs.mnt")
            .to_string_lossy()
            .to_string(),
        workspaces_path: stale_dir.join("workspaces").to_string_lossy().to_string(),
        home_base_path: stale_dir.join("home-base").to_string_lossy().to_string(),
        limits: SandboxLimits::default(),
        status: SandboxStatus::Stopped,
    };
    with_registry_mut(&state, |registry| {
        registry.sandboxes.insert(
            metadata.id.clone(),
            RegistrySandbox {
                metadata,
                workspaces: Default::default(),
            },
        );
        Ok(())
    })
    .expect("register stale sandbox");

    let report = repair_registry(&state, false).expect("repair stale state");
    assert_eq!(report.removed_sandboxes, 1);
    assert!(!stale_dir.exists());
    assert!(!orphan_dir.exists());
    with_registry(&state, |registry| {
        assert!(registry.sandboxes.is_empty());
        Ok(())
    })
    .expect("read repaired registry");

    let _ = fs::remove_dir_all(state);
}

#[test]
fn partial_write_requires_explicit_repair_before_mutation() {
    let state = state_dir("enclave-registry-partial");
    ensure_registry(&state).expect("registry init");

    fs::write(state.join("registry.json"), "{\"version\":").expect("write partial content");

    let err = with_registry_mut(&state, |registry| {
        registry.version = 1;
        Ok(())
    })
    .expect_err("mutating invalid registry should fail");
    assert!(err.to_string().contains("invalid registry"));

    let report = repair_registry(&state, false).expect("repair should recover from corruption");
    assert!(report.added_sandboxes == 0);

    let version = with_registry(&state, |registry| Ok(registry.version)).expect("registry read");
    assert_eq!(version, 1);
    let _ = fs::remove_dir_all(state);
}

#[test]
fn registry_cache_refreshes_after_external_atomic_replace() {
    let state = state_dir("enclave-registry-cache-refresh");
    ensure_registry(&state).expect("registry init");

    with_registry_mut(&state, |registry| {
        registry.version = 7;
        Ok(())
    })
    .expect("initial registry write");

    let observed = with_registry(&state, |registry| Ok(registry.version)).expect("cached read");
    assert_eq!(observed, 7);

    let replacement = serde_json::to_vec(&enclave::registry::Registry {
        version: 8,
        sandboxes: Default::default(),
    })
    .expect("serialize replacement");
    let replacement_path = state.join("registry.replacement");
    fs::write(&replacement_path, replacement).expect("write replacement");
    fs::rename(&replacement_path, state.join("registry.json")).expect("replace registry");

    let refreshed = with_registry(&state, |registry| Ok(registry.version)).expect("refresh read");
    assert_eq!(refreshed, 8);
    let _ = fs::remove_dir_all(state);
}
