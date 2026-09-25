use super::super::cleanup::cleanup_workspace_artifacts;
use super::*;
use std::collections::BTreeMap;
use std::fs;

use crate::registry::RegistrySandbox;
use crate::sandbox::{BootstrapMethod, SandboxLimits, SandboxMetadata, SandboxStatus};
use crate::workspace::types::NamespaceRefs;
use crate::workspace::{CleanupMode, WorkspaceLimits};

#[test]
fn cleanup_workspace_artifacts_removes_stale_workspace_resources() {
    let temp_dir =
        std::env::temp_dir().join(format!("enclave-workspace-cleanup-{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox_dir = temp_dir.join("sandbox");
    let workspace_dir = sandbox_dir.join("workspaces").join("workspace-id");
    for path in [
        workspace_dir.join("fs"),
        workspace_dir.join("ns"),
        workspace_dir.join("home-upper"),
        workspace_dir.join("home-work"),
        workspace_dir.join("home-merged"),
        workspace_dir.join("runtime"),
    ] {
        fs::create_dir_all(path).unwrap();
    }
    fs::write(workspace_dir.join("fs.img"), "image").unwrap();
    fs::write(workspace_dir.join("ns").join("mnt.ref"), "mnt").unwrap();
    fs::write(workspace_dir.join("ns").join("pid.ref"), "pid").unwrap();
    fs::write(workspace_dir.join("workspace.json"), "{}").unwrap();

    let sandbox = SandboxMetadata {
        id: "sandbox-id".to_string(),
        name: "sandbox".to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: crate::sandbox::BootstrapMethod::CachedRootfs,
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
    let workspace = WorkspaceMetadata {
        id: "workspace-id".to_string(),
        sandbox_id: sandbox.id.clone(),
        name: "workspace".to_string(),
        created_at: "2026-08-06T00:00:00Z".to_string(),
        workspace_path: workspace_dir.to_string_lossy().to_string(),
        filesystem_path: workspace_dir.join("fs").to_string_lossy().to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: sandbox.rootfs_path.clone(),
        overlay_home_base_path: sandbox.home_base_path.clone(),
        overlay_home_upper_path: workspace_dir
            .join("home-upper")
            .to_string_lossy()
            .to_string(),
        overlay_home_work_path: workspace_dir
            .join("home-work")
            .to_string_lossy()
            .to_string(),
        overlay_home_merged_path: workspace_dir
            .join("home-merged")
            .to_string_lossy()
            .to_string(),
        auth_providers: Vec::new(),
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: WorkspaceStatus::Stopped,
        runtime_pid: Some(u32::MAX),
        runtime_starttime_ticks: None,
        namespace_refs: NamespaceRefs::default(),
        clear_tmp_on_restart: false,
        limits: WorkspaceLimits::default(),
        assigned_ip: None,
    };

    cleanup_workspace_artifacts(&sandbox, &workspace, CleanupMode::Normal).unwrap();
    assert!(!workspace_dir.exists());

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn cleanup_workspace_artifacts_accepts_missing_workspace_root() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-missing-root-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox_dir = temp_dir.join("sandbox");
    fs::create_dir_all(&sandbox_dir).unwrap();

    let sandbox = SandboxMetadata {
        id: "sandbox-id".to_string(),
        name: "sandbox".to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: crate::sandbox::BootstrapMethod::CachedRootfs,
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
    let workspace = WorkspaceMetadata {
        id: "workspace-id".to_string(),
        sandbox_id: sandbox.id.clone(),
        name: "workspace".to_string(),
        created_at: "2026-08-06T00:00:00Z".to_string(),
        workspace_path: sandbox_dir
            .join("workspaces")
            .join("workspace-id")
            .to_string_lossy()
            .to_string(),
        filesystem_path: sandbox_dir
            .join("workspaces")
            .join("workspace-id")
            .join("fs")
            .to_string_lossy()
            .to_string(),
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
        status: WorkspaceStatus::Stopped,
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: NamespaceRefs::default(),
        clear_tmp_on_restart: false,
        limits: WorkspaceLimits::default(),
        assigned_ip: None,
    };

    cleanup_workspace_artifacts(&sandbox, &workspace, CleanupMode::Normal).unwrap();

    let _ = fs::remove_dir_all(&temp_dir);
}

/// Build a sandbox whose paths all live under `temp_dir`.
fn sandbox_metadata(temp_dir: &std::path::Path) -> SandboxMetadata {
    let sandbox_dir = temp_dir.join("sandbox");
    SandboxMetadata {
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
    }
}

/// A workspace record that claims `runtime_pid` as its runtime.
///
/// Passing this test process makes the teardown unable to stop the "runtime",
/// which is exactly the condition force mode exists to report.
fn workspace_metadata(
    sandbox: &SandboxMetadata,
    workspace_dir: &std::path::Path,
    runtime_pid: Option<u32>,
) -> WorkspaceMetadata {
    let runtime_starttime_ticks = runtime_pid
        .filter(|pid| *pid == std::process::id())
        .map(|pid| super::session::process_starttime_ticks(pid).unwrap());
    WorkspaceMetadata {
        id: "workspace-id".to_string(),
        sandbox_id: sandbox.id.clone(),
        name: "workspace".to_string(),
        created_at: "2026-08-06T00:00:00Z".to_string(),
        workspace_path: workspace_dir.to_string_lossy().to_string(),
        filesystem_path: workspace_dir.join("fs").to_string_lossy().to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: sandbox.rootfs_path.clone(),
        overlay_home_base_path: sandbox.home_base_path.clone(),
        overlay_home_upper_path: workspace_dir
            .join("home-upper")
            .to_string_lossy()
            .to_string(),
        overlay_home_work_path: workspace_dir
            .join("home-work")
            .to_string_lossy()
            .to_string(),
        overlay_home_merged_path: workspace_dir
            .join("home-merged")
            .to_string_lossy()
            .to_string(),
        auth_providers: Vec::new(),
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: WorkspaceStatus::Running,
        runtime_pid,
        runtime_starttime_ticks,
        namespace_refs: NamespaceRefs::default(),
        clear_tmp_on_restart: false,
        limits: WorkspaceLimits::default(),
        assigned_ip: None,
    }
}

#[test]
fn cleanup_workspace_artifacts_preserves_a_live_runtime_when_workspace_root_is_missing() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-orphan-runtime-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox = sandbox_metadata(&temp_dir);
    fs::create_dir_all(&sandbox.sandbox_path).unwrap();
    let pid = std::process::id();
    let starttime = super::session::process_starttime_ticks(pid).unwrap();
    let workspace_path = std::path::PathBuf::from(&sandbox.workspaces_path).join("workspace-id");
    let workspace = workspace_metadata(&sandbox, &workspace_path, Some(pid));

    let error = cleanup_workspace_artifacts(&sandbox, &workspace, CleanupMode::Normal)
        .expect_err("live PID evidence must block deletion when the workspace path is missing");
    assert!(error.to_string().contains("still alive after stop"));
    assert!(super::session::process_matches(pid, Some(starttime)));
    assert!(!workspace_path.exists());
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn cleanup_workspace_artifacts_retains_record_when_runtime_outlives_missing_root() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-live-missing-root-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox = sandbox_metadata(&temp_dir);
    fs::create_dir_all(&sandbox.sandbox_path).unwrap();
    let pid = std::process::id();
    let starttime = super::session::process_starttime_ticks(pid).unwrap();
    let workspace_path = std::path::PathBuf::from(&sandbox.workspaces_path).join("workspace-id");
    let workspace = workspace_metadata(&sandbox, &workspace_path, Some(pid));

    let error = cleanup_workspace_artifacts(&sandbox, &workspace, CleanupMode::Normal)
        .expect_err("a live runtime must block cleanup after its workspace root disappears");
    assert!(error.to_string().contains("still alive after stop"));
    assert!(super::session::process_matches(pid, Some(starttime)));
    assert!(!workspace_path.exists());
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn force_cleanup_reports_a_live_runtime_and_keeps_the_workspace_files() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-force-cleanup-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox = sandbox_metadata(&temp_dir);
    let workspace_path = std::path::PathBuf::from(&sandbox.workspaces_path).join("workspace-id");
    fs::create_dir_all(workspace_path.join("fs")).unwrap();
    fs::write(workspace_path.join("fs.img"), "image").unwrap();
    fs::write(workspace_path.join("workspace.json"), "{}").unwrap();
    let workspace = workspace_metadata(&sandbox, &workspace_path, Some(std::process::id()));

    let outcome = cleanup_workspace_artifacts(&sandbox, &workspace, CleanupMode::Force)
        .expect("force cleanup must report the live runtime instead of failing");
    assert!(!outcome.is_complete());
    assert!(!outcome.files_removed);
    assert!(outcome.retained.iter().any(|item| {
        item.resource == "runtime" && item.detail.contains("still alive after stop")
    }));
    // The files are the only remaining description of what is still running, so
    // force mode reports them and leaves them in place.
    assert!(workspace_path.join("fs.img").exists());
    assert!(workspace_path.join("workspace.json").exists());
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn force_destroy_drops_the_record_and_reports_the_live_runtime() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-force-destroy-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    crate::registry::ensure_registry(&temp_dir).unwrap();
    let sandbox = sandbox_metadata(&temp_dir);
    let workspace_path = std::path::PathBuf::from(&sandbox.workspaces_path).join("workspace-id");
    fs::create_dir_all(workspace_path.join("fs")).unwrap();
    fs::write(workspace_path.join("fs.img"), "image").unwrap();
    let workspace = workspace_metadata(&sandbox, &workspace_path, Some(std::process::id()));
    with_registry_mut(&temp_dir, |registry| {
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

    let error =
        destroy_workspace_with_mode(&temp_dir, &sandbox.id, &workspace.id, CleanupMode::Normal)
            .expect_err("normal mode must keep the record while a runtime is still alive");
    assert!(format!("{error:#}").contains("is still alive after stop"));
    with_registry(&temp_dir, |registry| {
        assert!(registry.sandboxes[&sandbox.id]
            .workspaces
            .contains_key(&workspace.id));
        Ok(())
    })
    .unwrap();

    let report =
        destroy_workspace_with_mode(&temp_dir, &sandbox.id, &workspace.id, CleanupMode::Force)
            .expect("force mode must report the retained runtime instead of failing");
    assert_eq!(report.workspace_id, workspace.id);
    assert!(report.mode.is_force());
    assert!(report
        .retained
        .iter()
        .any(|item| item.resource == "runtime"));
    with_registry(&temp_dir, |registry| {
        assert!(!registry.sandboxes[&sandbox.id]
            .workspaces
            .contains_key(&workspace.id));
        Ok(())
    })
    .unwrap();
    assert!(workspace_path.join("fs.img").exists());
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn destroy_all_workspaces_returns_empty_plan_without_spawning_cleanup_workers() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-batch-empty-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);

    crate::registry::ensure_registry(&temp_dir).unwrap();
    let report = destroy_all_workspaces(&temp_dir, CleanupMode::Normal).unwrap();

    assert!(report.removed.is_empty());
    assert!(report.errors.is_empty());
    let _ = fs::remove_dir_all(&temp_dir);
}

/// A workspace that is starting already owns its address.
///
/// The address is reserved under the registry lock so two workspaces starting at
/// the same time cannot both take the first free one. Counting only running
/// workspaces would let the second one take the address the first just reserved,
/// which is how the batch start path ended up with several workspaces sharing one
/// address.
#[test]
fn used_addresses_include_a_workspace_that_is_only_starting() {
    let mut registry = crate::registry::Registry::default();
    let mut sandbox = crate::registry::RegistrySandbox {
        metadata: sandbox_metadata(std::path::Path::new("/tmp/enclave-ipam-test")),
        workspaces: BTreeMap::new(),
    };
    let mut workspace = workspace_metadata(
        &sandbox.metadata,
        std::path::Path::new("/tmp/enclave-ipam-test/sandboxes/sandbox/workspaces/ws"),
        None,
    );
    workspace.status = WorkspaceStatus::Starting;
    workspace.assigned_ip = Some("10.200.0.10".to_string());
    sandbox.workspaces.insert(workspace.id.clone(), workspace);
    registry
        .sandboxes
        .insert(sandbox.metadata.id.clone(), sandbox);

    let used = collect_all_used_ip_octets(&registry);
    assert!(
        used.contains(&10),
        "a starting workspace's reserved address must not be handed out again"
    );
}

/// A workspace whose runtime died must not keep the interface, rules, or mounts
/// the runtime owned.
///
/// The record is the only description of what to release, so reconcile releases
/// first and clears the record second. When the release cannot be proven — for
/// example without privileges to read the firewall — the record must survive, so
/// the resources stay findable instead of being silently orphaned.
#[test]
fn reconcile_releases_host_resources_for_a_dead_runtime() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-reconcile-dead-runtime-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox = sandbox_metadata(&temp_dir);
    fs::create_dir_all(&sandbox.sandbox_path).unwrap();
    let workspace_dir = std::path::PathBuf::from(&sandbox.workspaces_path).join("workspace-id");
    fs::create_dir_all(workspace_dir.join("ns")).unwrap();

    let mut workspace = workspace_metadata(&sandbox, &workspace_dir, None);
    workspace.status = WorkspaceStatus::Running;
    workspace.runtime_pid = Some(u32::MAX);
    workspace.runtime_starttime_ticks = Some(1);
    workspace.assigned_ip = Some("10.200.0.42".to_string());

    let repaired = reconcile_workspace_runtime_state(&sandbox, &mut workspace).unwrap();
    if repaired {
        assert_eq!(workspace.status, WorkspaceStatus::Stopped);
        assert!(workspace.runtime_pid.is_none());
        assert!(workspace.runtime_starttime_ticks.is_none());
        assert!(workspace.assigned_ip.is_none());
    } else {
        assert_eq!(workspace.status, WorkspaceStatus::Running);
        assert_eq!(workspace.runtime_pid, Some(u32::MAX));
        assert_eq!(workspace.assigned_ip.as_deref(), Some("10.200.0.42"));
    }
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn reconcile_clears_dead_runtime_and_namespace_references() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-runtime-reconcile-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox_dir = temp_dir.join("sandbox");
    let workspace_dir = sandbox_dir.join("workspaces").join("workspace-id");
    fs::create_dir_all(workspace_dir.join("ns")).unwrap();
    fs::write(workspace_dir.join("ns").join("mnt.ref"), "mnt:[1]\n").unwrap();
    fs::write(workspace_dir.join("ns").join("pid.ref"), "pid:[1]\n").unwrap();

    let mut workspace = WorkspaceMetadata {
        id: "workspace-id".to_string(),
        sandbox_id: "sandbox-id".to_string(),
        name: "workspace".to_string(),
        created_at: "2026-08-06T00:00:00Z".to_string(),
        workspace_path: workspace_dir.to_string_lossy().to_string(),
        filesystem_path: workspace_dir.join("fs").to_string_lossy().to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: sandbox_dir.join("rootfs").to_string_lossy().to_string(),
        overlay_home_base_path: sandbox_dir.join("home-base").to_string_lossy().to_string(),
        overlay_home_upper_path: String::new(),
        overlay_home_work_path: String::new(),
        overlay_home_merged_path: String::new(),
        auth_providers: Vec::new(),
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: WorkspaceStatus::Running,
        runtime_pid: Some(u32::MAX),
        runtime_starttime_ticks: Some(1),
        namespace_refs: NamespaceRefs {
            mount: workspace_dir
                .join("ns")
                .join("mnt.ref")
                .to_string_lossy()
                .to_string(),
            pid: workspace_dir
                .join("ns")
                .join("pid.ref")
                .to_string_lossy()
                .to_string(),
        },
        clear_tmp_on_restart: false,
        limits: WorkspaceLimits::default(),
        assigned_ip: Some("10.88.0.99".to_string()),
    };

    assert!(
        reconcile_workspace_runtime_state(&reconcile_sandbox(&sandbox_dir), &mut workspace)
            .unwrap()
    );
    assert_eq!(workspace.status, WorkspaceStatus::Stopped);
    assert!(workspace.runtime_pid.is_none());
    assert!(workspace.runtime_starttime_ticks.is_none());
    assert!(workspace.assigned_ip.is_none());
    assert_eq!(workspace.namespace_refs.mount, "unassigned");
    assert_eq!(workspace.namespace_refs.pid, "unassigned");
    assert!(!workspace_dir.join("ns").join("mnt.ref").exists());
    assert!(!workspace_dir.join("ns").join("pid.ref").exists());

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn workspace_status_helpers_partition_lifecycle_states() {
    for status in [WorkspaceStatus::Starting, WorkspaceStatus::Stopping] {
        assert!(status.is_transitional(), "{status:?} is transitional");
        assert!(!status.is_running(), "{status:?} is not usable");
        assert!(status.may_have_runtime(), "{status:?} may own a runtime");
    }
    assert!(WorkspaceStatus::Running.is_running());
    assert!(!WorkspaceStatus::Running.is_transitional());
    assert!(WorkspaceStatus::Running.may_have_runtime());
    assert!(!WorkspaceStatus::Stopped.may_have_runtime());
    assert!(!WorkspaceStatus::Stopped.is_running());
    assert!(!WorkspaceStatus::Stopped.is_transitional());
    assert_eq!(WorkspaceStatus::Starting.as_str(), "starting");
    assert_eq!(WorkspaceStatus::Stopping.as_str(), "stopping");
    assert_eq!(WorkspaceStatus::Running.as_str(), "running");
    assert_eq!(WorkspaceStatus::Stopped.as_str(), "stopped");
}

fn transitional_workspace(
    sandbox_dir: &std::path::Path,
    status: WorkspaceStatus,
    runtime: Option<(u32, u64)>,
) -> (WorkspaceMetadata, std::path::PathBuf) {
    let workspace_dir = sandbox_dir.join("workspaces").join("workspace-id");
    fs::create_dir_all(workspace_dir.join("ns")).unwrap();
    fs::write(workspace_dir.join("ns").join("mnt.ref"), "mnt:[1]\n").unwrap();
    fs::write(workspace_dir.join("ns").join("pid.ref"), "pid:[1]\n").unwrap();
    let workspace = WorkspaceMetadata {
        id: "workspace-id".to_string(),
        sandbox_id: "sandbox-id".to_string(),
        name: "workspace".to_string(),
        created_at: "2026-08-06T00:00:00Z".to_string(),
        workspace_path: workspace_dir.to_string_lossy().to_string(),
        filesystem_path: workspace_dir.join("fs").to_string_lossy().to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: sandbox_dir.join("rootfs").to_string_lossy().to_string(),
        overlay_home_base_path: sandbox_dir.join("home-base").to_string_lossy().to_string(),
        overlay_home_upper_path: String::new(),
        overlay_home_work_path: String::new(),
        overlay_home_merged_path: String::new(),
        auth_providers: Vec::new(),
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status,
        runtime_pid: runtime.map(|(pid, _)| pid),
        runtime_starttime_ticks: runtime.map(|(_, starttime)| starttime),
        namespace_refs: NamespaceRefs {
            mount: workspace_dir
                .join("ns")
                .join("mnt.ref")
                .to_string_lossy()
                .to_string(),
            pid: workspace_dir
                .join("ns")
                .join("pid.ref")
                .to_string_lossy()
                .to_string(),
        },
        clear_tmp_on_restart: false,
        limits: WorkspaceLimits::default(),
        assigned_ip: None,
    };
    (workspace, workspace_dir)
}

fn reconcile_sandbox(sandbox_dir: &std::path::Path) -> SandboxMetadata {
    SandboxMetadata {
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
        status: SandboxStatus::Running,
    }
}

#[test]
fn reconcile_rolls_back_interrupted_start_without_a_live_runtime() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-reconcile-starting-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox_dir = temp_dir.join("sandbox");
    let (mut workspace, workspace_dir) =
        transitional_workspace(&sandbox_dir, WorkspaceStatus::Starting, None);

    assert!(
        reconcile_workspace_runtime_state(&reconcile_sandbox(&sandbox_dir), &mut workspace)
            .unwrap()
    );
    assert_eq!(workspace.status, WorkspaceStatus::Stopped);
    assert!(workspace.runtime_pid.is_none());
    assert!(workspace.runtime_starttime_ticks.is_none());
    assert!(workspace.assigned_ip.is_none());
    assert_eq!(workspace.namespace_refs.mount, "unassigned");
    assert!(!workspace_dir.join("ns").join("mnt.ref").exists());
    assert!(!workspace_dir.join("ns").join("pid.ref").exists());
    // The rollback is persisted so a second reconcile has nothing left to do.
    assert!(
        !reconcile_workspace_runtime_state(&reconcile_sandbox(&sandbox_dir), &mut workspace)
            .unwrap()
    );

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn reconcile_rolls_back_interrupted_stop_with_a_dead_runtime() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-reconcile-stopping-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox_dir = temp_dir.join("sandbox");
    let (mut workspace, workspace_dir) =
        transitional_workspace(&sandbox_dir, WorkspaceStatus::Stopping, Some((u32::MAX, 1)));

    assert!(
        reconcile_workspace_runtime_state(&reconcile_sandbox(&sandbox_dir), &mut workspace)
            .unwrap()
    );
    assert_eq!(workspace.status, WorkspaceStatus::Stopped);
    assert!(workspace.runtime_pid.is_none());
    assert!(workspace.runtime_starttime_ticks.is_none());
    assert!(!workspace_dir.join("ns").join("mnt.ref").exists());

    let _ = fs::remove_dir_all(&temp_dir);
}
