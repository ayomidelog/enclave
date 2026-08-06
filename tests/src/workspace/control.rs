use super::*;
use std::fs;

use crate::sandbox::{SandboxLimits, SandboxStatus};
use crate::workspace::types::NamespaceRefs;
use crate::workspace::WorkspaceLimits;

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
        limits: WorkspaceLimits::default(),
        assigned_ip: None,
    };

    cleanup_workspace_artifacts(&sandbox, &workspace).unwrap();
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
        limits: WorkspaceLimits::default(),
        assigned_ip: None,
    };

    cleanup_workspace_artifacts(&sandbox, &workspace).unwrap();

    let _ = fs::remove_dir_all(&temp_dir);
}
