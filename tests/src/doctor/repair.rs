//! What a repair is allowed to change, and what it must report instead.

use super::*;

#[test]
fn repair_removes_orphaned_sandbox_directories_and_validates_daemon_state() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-doctor-repair-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let socket_path = temp_dir.join("daemon.sock");
    std::fs::create_dir_all(temp_dir.join("sandboxes").join("orphan")).unwrap();

    let state_lock = crate::daemon::state_lock::acquire_state_lock(&temp_dir, &socket_path)
        .expect("acquire daemon state lock");
    let report = repair_doctor(&temp_dir, &socket_path).expect("repair state");

    assert!(report.daemon_state_consistent);
    assert!(!temp_dir.join("sandboxes").join("orphan").exists());

    drop(state_lock);
    let _ = std::fs::remove_dir_all(&temp_dir);
}

/// The doctor is read-only, so it is where an operator looks before anything is
/// resolved. A workspace whose registry record and on-disk metadata have diverged
/// must show up here: repair would adopt the on-disk copy, and the operator needs
/// to know that before it happens rather than afterwards.
#[test]
fn doctor_reports_a_workspace_whose_metadata_copies_disagree() {
    use std::collections::BTreeMap;
    use std::fs;

    let state_dir = std::env::temp_dir().join(format!(
        "enclave-doctor-disagreement-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = fs::remove_dir_all(&state_dir);

    let sandbox_dir = state_dir.join("sandboxes").join("sb-doctor");
    let workspace_dir = sandbox_dir.join("workspaces").join("ws-doctor");
    fs::create_dir_all(workspace_dir.join("fs")).expect("create workspace dir");
    fs::create_dir_all(sandbox_dir.join("rootfs")).expect("create rootfs dir");

    let sandbox = crate::sandbox::SandboxMetadata {
        id: "sb-doctor".to_string(),
        name: "sb-doctor".to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: crate::sandbox::BootstrapMethod::CachedRootfs,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        sandbox_path: sandbox_dir.to_string_lossy().to_string(),
        rootfs_path: sandbox_dir.join("rootfs").to_string_lossy().to_string(),
        rootfs_lower_path: None,
        mounted_rootfs_path: sandbox_dir
            .join("runtime/rootfs.mnt")
            .to_string_lossy()
            .to_string(),
        workspaces_path: sandbox_dir.join("workspaces").to_string_lossy().to_string(),
        home_base_path: sandbox_dir.join("home-base").to_string_lossy().to_string(),
        limits: crate::sandbox::SandboxLimits::default(),
        status: crate::sandbox::SandboxStatus::Stopped,
    };
    fs::write(
        sandbox_dir.join("sandbox.json"),
        serde_json::to_string_pretty(&sandbox).expect("serialize sandbox"),
    )
    .expect("write sandbox metadata");

    let workspace = crate::workspace::WorkspaceMetadata {
        id: "ws-doctor".to_string(),
        sandbox_id: sandbox.id.clone(),
        name: "doctor".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        workspace_path: workspace_dir.to_string_lossy().to_string(),
        filesystem_path: workspace_dir.join("fs").to_string_lossy().to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: sandbox.rootfs_path.clone(),
        overlay_home_base_path: sandbox.home_base_path.clone(),
        overlay_home_upper_path: String::new(),
        overlay_home_work_path: String::new(),
        overlay_home_merged_path: String::new(),
        auth_providers: Vec::new(),
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: crate::workspace::WorkspaceStatus::Stopped,
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: Default::default(),
        clear_tmp_on_restart: false,
        limits: crate::workspace::WorkspaceLimits::default(),
        assigned_ip: None,
    };

    // The file claims a running workspace; the registry record says stopped.
    let mut on_disk = workspace.clone();
    on_disk.status = crate::workspace::WorkspaceStatus::Running;
    on_disk.runtime_pid = Some(4242);
    fs::write(
        workspace_dir.join("workspace.json"),
        serde_json::to_string_pretty(&on_disk).expect("serialize workspace"),
    )
    .expect("write workspace metadata");

    crate::registry::with_registry_mut(&state_dir, |registry| {
        registry.sandboxes.insert(
            sandbox.id.clone(),
            crate::registry::RegistrySandbox {
                metadata: sandbox.clone(),
                workspaces: BTreeMap::from([(workspace.id.clone(), workspace.clone())]),
            },
        );
        Ok(())
    })
    .expect("seed the registry");

    let check = crate::doctor::registry::check_registry_consistency(&state_dir);
    assert_eq!(check.status, "warn", "{}", check.detail);
    assert!(
        check.detail.contains("ws-doctor") && check.detail.contains("disagrees"),
        "{}",
        check.detail
    );
    for field in ["status", "runtime_pid"] {
        assert!(check.detail.contains(field), "{}", check.detail);
    }

    let _ = fs::remove_dir_all(&state_dir);
}
