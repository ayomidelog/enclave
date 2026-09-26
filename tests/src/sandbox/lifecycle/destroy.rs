//! What a destroy has to prove before it drops a record.
//!
//! A destroy removes the only description of what the sandbox owned, so these pin
//! that it keeps the record when there is still something to release or still a
//! runtime using it.

use super::*;

#[test]
fn destroy_keeps_registry_entry_until_unmount_cleanup_succeeds() {
    let temp_dir =
        std::env::temp_dir().join(format!("enclave-destroy-registry-{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox_dir = temp_dir.join("sandboxes").join("sandbox-id");
    fs::create_dir_all(sandbox_dir.join("runtime")).unwrap();
    fs::create_dir_all(&temp_dir).unwrap();

    let metadata = SandboxMetadata {
        id: "sandbox-id".to_string(),
        name: "sandbox".to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: BootstrapMethod::CachedRootfs,
        created_at: "2026-08-06T00:00:00Z".to_string(),
        sandbox_path: sandbox_dir.to_string_lossy().to_string(),
        rootfs_path: sandbox_dir.join("rootfs").to_string_lossy().to_string(),
        rootfs_lower_path: None,
        mounted_rootfs_path: sandbox_dir
            .join("runtime")
            .join("rootfs.mnt")
            .to_string_lossy()
            .to_string(),
        workspaces_path: sandbox_dir.join("workspaces").to_string_lossy().to_string(),
        home_base_path: sandbox_dir.join("home-base").to_string_lossy().to_string(),
        limits: SandboxLimits::default(),
        status: SandboxStatus::Stopped,
    };
    std::os::unix::fs::symlink("/tmp", &metadata.mounted_rootfs_path).unwrap();

    with_registry_mut(&temp_dir, |registry| {
        registry.sandboxes.insert(
            metadata.id.clone(),
            RegistrySandbox {
                metadata: metadata.clone(),
                workspaces: BTreeMap::new(),
            },
        );
        Ok(())
    })
    .unwrap();

    let error = destroy_sandbox(&temp_dir, "sandbox").unwrap_err();
    assert!(format!("{error:#}").contains("must not be a symlink"));

    with_registry(&temp_dir, |registry| {
        assert!(registry.sandboxes.contains_key("sandbox-id"));
        Ok(())
    })
    .unwrap();

    fs::remove_file(&metadata.mounted_rootfs_path).unwrap();
    destroy_sandbox(&temp_dir, "sandbox").unwrap();
    with_registry(&temp_dir, |registry| {
        assert!(!registry.sandboxes.contains_key("sandbox-id"));
        Ok(())
    })
    .unwrap();

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn destroy_with_missing_sandbox_path_preserves_live_workspace_runtime_record() {
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-destroy-live-orphan-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&state_dir);
    fs::create_dir_all(&state_dir).unwrap();
    let sandbox_path = state_dir.join("sandboxes/sandbox-id");
    let workspace_path = sandbox_path.join("workspaces/workspace-id");
    let sandbox = SandboxMetadata {
        id: "sandbox-id".to_string(),
        name: "sandbox".to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: BootstrapMethod::CachedRootfs,
        created_at: "2026-08-06T00:00:00Z".to_string(),
        sandbox_path: sandbox_path.to_string_lossy().to_string(),
        rootfs_path: sandbox_path.join("rootfs").to_string_lossy().to_string(),
        rootfs_lower_path: None,
        mounted_rootfs_path: sandbox_path
            .join("runtime/rootfs.mnt")
            .to_string_lossy()
            .to_string(),
        workspaces_path: sandbox_path
            .join("workspaces")
            .to_string_lossy()
            .to_string(),
        home_base_path: sandbox_path.join("home-base").to_string_lossy().to_string(),
        limits: SandboxLimits::default(),
        status: SandboxStatus::Stopped,
    };
    let runtime_pid = std::process::id();
    let runtime_starttime_ticks = crate::workspace::session::process_starttime_ticks(runtime_pid)
        .expect("current test process start time");
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
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: WorkspaceStatus::Running,
        runtime_pid: Some(runtime_pid),
        runtime_starttime_ticks: Some(runtime_starttime_ticks),
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
                workspaces: BTreeMap::from([(workspace.id.clone(), workspace)]),
            },
        );
        Ok(())
    })
    .unwrap();

    let error = destroy_sandbox(&state_dir, &sandbox.id)
        .expect_err("destroy must retain live runtime evidence without its sandbox directory");
    assert!(format!("{error:#}").contains("is still alive"));
    with_registry(&state_dir, |registry| {
        assert!(registry.sandboxes.contains_key(&sandbox.id));
        assert!(registry.sandboxes[&sandbox.id]
            .workspaces
            .contains_key("workspace-id"));
        Ok(())
    })
    .unwrap();
    let _ = fs::remove_dir_all(&state_dir);
}

#[test]
fn destroy_with_live_workspace_retains_sandbox_and_registry() {
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-destroy-live-workspace-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&state_dir);
    let sandbox_path = state_dir.join("sandboxes/sandbox-id");
    let workspace_path = sandbox_path.join("workspaces/workspace-id");
    fs::create_dir_all(workspace_path.join("fs")).unwrap();
    fs::create_dir_all(workspace_path.join("ns")).unwrap();
    fs::create_dir_all(workspace_path.join("runtime")).unwrap();
    fs::create_dir_all(sandbox_path.join("rootfs")).unwrap();

    let sandbox = SandboxMetadata {
        id: "sandbox-id".to_string(),
        name: "sandbox".to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: BootstrapMethod::CachedRootfs,
        created_at: "2026-08-06T00:00:00Z".to_string(),
        sandbox_path: sandbox_path.to_string_lossy().to_string(),
        rootfs_path: sandbox_path.join("rootfs").to_string_lossy().to_string(),
        rootfs_lower_path: None,
        mounted_rootfs_path: sandbox_path
            .join("runtime/rootfs.mnt")
            .to_string_lossy()
            .to_string(),
        workspaces_path: sandbox_path
            .join("workspaces")
            .to_string_lossy()
            .to_string(),
        home_base_path: sandbox_path.join("home-base").to_string_lossy().to_string(),
        limits: SandboxLimits::default(),
        status: SandboxStatus::Stopped,
    };
    let runtime_pid = std::process::id();
    let runtime_starttime_ticks = crate::workspace::session::process_starttime_ticks(runtime_pid)
        .expect("current test process start time");
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
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: WorkspaceStatus::Running,
        runtime_pid: Some(runtime_pid),
        runtime_starttime_ticks: Some(runtime_starttime_ticks),
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
                workspaces: BTreeMap::from([(workspace.id.clone(), workspace)]),
            },
        );
        Ok(())
    })
    .unwrap();

    let error = destroy_sandbox(&state_dir, &sandbox.id)
        .expect_err("sandbox destroy must refuse a live runtime it cannot safely signal");
    assert!(format!("{error:#}").contains("is still alive"));
    assert!(workspace_path.exists());
    assert!(sandbox_path.join("rootfs").exists());
    with_registry(&state_dir, |registry| {
        assert!(registry.sandboxes[&sandbox.id]
            .workspaces
            .contains_key("workspace-id"));
        Ok(())
    })
    .unwrap();
    let _ = fs::remove_dir_all(&state_dir);
}
