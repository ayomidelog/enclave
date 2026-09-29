//! Rolling an interrupted sandbox operation back, and the states that must not be
//! rolled back.

use super::*;

#[test]
fn reconcile_rolls_back_interrupted_sandbox_transitions() {
    let state_dir =
        std::env::temp_dir().join(format!("enclave-sandbox-reconcile-{}", std::process::id()));
    let _ = fs::remove_dir_all(&state_dir);
    crate::registry::ensure_registry(&state_dir).unwrap();

    let starting = transitional_sandbox(&state_dir, "sandbox-starting", SandboxStatus::Starting);
    let stopping = transitional_sandbox(&state_dir, "sandbox-stopping", SandboxStatus::Stopping);
    let running = transitional_sandbox(&state_dir, "sandbox-running", SandboxStatus::Running);
    with_registry_mut(&state_dir, |registry| {
        for metadata in [&starting, &stopping, &running] {
            registry.sandboxes.insert(
                metadata.id.clone(),
                RegistrySandbox {
                    metadata: metadata.clone(),
                    workspaces: BTreeMap::new(),
                },
            );
        }
        Ok(())
    })
    .unwrap();

    reconcile_sandbox_states(&state_dir).unwrap();

    with_registry(&state_dir, |registry| {
        assert_eq!(
            registry.sandboxes[&starting.id].metadata.status,
            SandboxStatus::Stopped
        );
        assert_eq!(
            registry.sandboxes[&stopping.id].metadata.status,
            SandboxStatus::Stopped
        );
        // A settled sandbox is never touched by the reconcile pass.
        assert_eq!(
            registry.sandboxes[&running.id].metadata.status,
            SandboxStatus::Running
        );
        Ok(())
    })
    .unwrap();

    let _ = fs::remove_dir_all(&state_dir);
}

#[test]
fn reconcile_runtime_state_reports_every_repaired_record() {
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-runtime-reconcile-count-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&state_dir);
    crate::registry::ensure_registry(&state_dir).unwrap();

    let sandbox = transitional_sandbox(&state_dir, "sandbox-stopping", SandboxStatus::Stopping);
    let workspace_path = std::path::PathBuf::from(&sandbox.sandbox_path)
        .join("workspaces")
        .join("workspace-id");
    fs::create_dir_all(&workspace_path).unwrap();
    let workspace = WorkspaceMetadata {
        id: "workspace-id".to_string(),
        sandbox_id: sandbox.id.clone(),
        name: "workspace".to_string(),
        created_at: "2026-08-06T00:00:00Z".to_string(),
        workspace_path: workspace_path.to_string_lossy().to_string(),
        filesystem_path: workspace_path.join("fs").to_string_lossy().to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: sandbox.rootfs_path.clone(),
        overlay_home_base_path: sandbox.home_base_path.clone(),
        overlay_home_upper_path: String::new(),
        overlay_home_work_path: String::new(),
        overlay_home_merged_path: String::new(),
        auth_providers: Vec::new(),
        owner: None,
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: WorkspaceStatus::Stopping,
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: Default::default(),
        clear_tmp_on_restart: false,
        limits: WorkspaceLimits::default(),
        assigned_ip: None,
    };
    with_registry_mut(&state_dir, |registry| {
        registry.sandboxes.insert(
            sandbox.id.clone(),
            RegistrySandbox {
                metadata: sandbox.clone(),
                workspaces: BTreeMap::from([(workspace.id.clone(), workspace.clone())]),
            },
        );
        Ok(())
    })
    .unwrap();

    assert_eq!(reconcile_runtime_state(&state_dir).unwrap(), 2);
    with_registry(&state_dir, |registry| {
        assert_eq!(
            registry.sandboxes[&sandbox.id].metadata.status,
            SandboxStatus::Stopped
        );
        assert_eq!(
            registry.sandboxes[&sandbox.id].workspaces[&workspace.id].status,
            WorkspaceStatus::Stopped
        );
        Ok(())
    })
    .unwrap();
    // A repaired record is not reported twice.
    assert_eq!(reconcile_runtime_state(&state_dir).unwrap(), 0);

    let _ = fs::remove_dir_all(&state_dir);
}

/// `init_storage` runs on every create, and a create must not resolve a
/// transition another request has in flight. A workspace is marked `Starting`
/// before its runtime is launched and only committed afterwards, so reconciling
/// from `init_storage` would reset it to `Stopped` underneath the launch and
/// make the launch refuse to commit its runtime.
#[test]
fn init_storage_does_not_roll_back_an_in_flight_start() {
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-init-storage-transition-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&state_dir);

    let sandbox = transitional_sandbox(&state_dir, "sandbox-live", SandboxStatus::Stopped);
    // Repair drops a sandbox whose rootfs is missing, and `init_storage` runs it.
    fs::create_dir_all(std::path::PathBuf::from(&sandbox.rootfs_path)).unwrap();
    let workspace_path = std::path::PathBuf::from(&sandbox.workspaces_path).join("workspace-id");
    fs::create_dir_all(workspace_path.join("fs")).unwrap();
    let workspace = WorkspaceMetadata {
        id: "workspace-id".to_string(),
        sandbox_id: sandbox.id.clone(),
        name: "workspace".to_string(),
        created_at: "2026-08-06T00:00:00Z".to_string(),
        workspace_path: workspace_path.to_string_lossy().to_string(),
        filesystem_path: workspace_path.join("fs").to_string_lossy().to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: sandbox.rootfs_path.clone(),
        overlay_home_base_path: sandbox.home_base_path.clone(),
        overlay_home_upper_path: workspace_path
            .join("home-upper")
            .to_string_lossy()
            .to_string(),
        overlay_home_work_path: workspace_path
            .join("home-work")
            .to_string_lossy()
            .to_string(),
        overlay_home_merged_path: workspace_path
            .join("home-merged")
            .to_string_lossy()
            .to_string(),
        auth_providers: Vec::new(),
        owner: None,
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        // The launch is about to write the runtime markers; this is exactly the
        // window a concurrent create must not touch. No address is recorded so the
        // later rollback does not reach for host networking in a unit test.
        status: WorkspaceStatus::Starting,
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: Default::default(),
        clear_tmp_on_restart: false,
        limits: WorkspaceLimits::default(),
        assigned_ip: None,
    };
    fs::write(
        std::path::PathBuf::from(&sandbox.sandbox_path).join("sandbox.json"),
        serde_json::to_string_pretty(&sandbox).unwrap(),
    )
    .unwrap();
    fs::write(
        workspace_path.join("workspace.json"),
        serde_json::to_string_pretty(&workspace).unwrap(),
    )
    .unwrap();

    init_storage(&state_dir).unwrap();
    with_registry(&state_dir, |registry| {
        assert_eq!(
            registry.sandboxes[&sandbox.id].workspaces[&workspace.id].status,
            WorkspaceStatus::Starting,
            "init_storage rolled back a start that was still in flight"
        );
        Ok(())
    })
    .unwrap();

    // The explicit reconciliation still resolves it, which is what recovers a
    // transition a previous daemon left behind.
    assert_eq!(reconcile_runtime_state(&state_dir).unwrap(), 1);
    with_registry(&state_dir, |registry| {
        assert_eq!(
            registry.sandboxes[&sandbox.id].workspaces[&workspace.id].status,
            WorkspaceStatus::Stopped
        );
        Ok(())
    })
    .unwrap();

    let _ = fs::remove_dir_all(&state_dir);
}
