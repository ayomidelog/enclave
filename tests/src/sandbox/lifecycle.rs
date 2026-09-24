use super::*;
use std::collections::BTreeMap;
use std::fs;

use crate::registry::{with_registry, with_registry_mut, RegistrySandbox};
use crate::sandbox::{BootstrapMethod, SandboxLimits, SandboxStatus};
use crate::workspace::{WorkspaceLimits, WorkspaceMetadata, WorkspaceStatus};

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

fn transitional_sandbox(
    state_dir: &std::path::Path,
    id: &str,
    status: SandboxStatus,
) -> SandboxMetadata {
    let sandbox_path = state_dir.join("sandboxes").join(id);
    fs::create_dir_all(&sandbox_path).unwrap();
    SandboxMetadata {
        id: id.to_string(),
        name: id.to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: BootstrapMethod::CachedRootfs,
        created_at: "2026-08-06T00:00:00Z".to_string(),
        sandbox_path: sandbox_path.to_string_lossy().to_string(),
        rootfs_path: sandbox_path.join("rootfs").to_string_lossy().to_string(),
        rootfs_lower_path: None,
        mounted_rootfs_path: sandbox_path
            .join("runtime")
            .join("rootfs.mnt")
            .to_string_lossy()
            .to_string(),
        workspaces_path: sandbox_path
            .join("workspaces")
            .to_string_lossy()
            .to_string(),
        home_base_path: sandbox_path.join("home-base").to_string_lossy().to_string(),
        limits: SandboxLimits::default(),
        status,
    }
}

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
    assert!(format!("{error:#}").contains("runtime pid"));
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
