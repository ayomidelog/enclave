//! Tests for the workspace lifecycle control.
//!
//! The fixtures are here because the submodules share them: a sandbox and a workspace
//! record, and the two shapes a record is left in when an operation is interrupted.

use super::super::cleanup::cleanup_workspace_artifacts;
use super::*;
use crate::registry::RegistrySandbox;
use crate::sandbox::{BootstrapMethod, SandboxLimits, SandboxMetadata, SandboxStatus};
use crate::workspace::types::NamespaceRefs;
use crate::workspace::{CleanupMode, WorkspaceLimits};
use std::collections::BTreeMap;
use std::fs;

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
        owner: None,
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
        owner: None,
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

mod cleanup;
mod force;
mod reconcile;
mod update;
