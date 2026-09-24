use super::*;
use std::fs;

use crate::network::NetworkCleanupReport;
use crate::sandbox::{BootstrapMethod, SandboxLimits, SandboxMetadata, SandboxStatus};
use crate::workspace::types::{NamespaceRefs, WorkspaceLimits, WorkspaceMetadata};
use crate::workspace::WorkspaceStatus;

fn fixture(
    label: &str,
    status: WorkspaceStatus,
    runtime: Option<(u32, u64)>,
) -> (WorkspaceMetadata, PathBuf) {
    // Each test needs its own state directory: they run in parallel threads and
    // share a process id.
    let root = std::env::temp_dir().join(format!(
        "enclave-workspace-certificate-{}-{label}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let sandbox_dir = root.join("sandbox");
    let workspace_dir = sandbox_dir.join("workspaces").join("workspace-id");
    fs::create_dir_all(workspace_dir.join("runtime")).unwrap();
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
        namespace_refs: NamespaceRefs::default(),
        clear_tmp_on_restart: false,
        limits: WorkspaceLimits::default(),
        assigned_ip: None,
    };
    (workspace, root)
}

#[test]
fn a_stopped_workspace_with_no_leftovers_has_a_complete_certificate() {
    let (workspace, root) = fixture("clean", WorkspaceStatus::Stopped, None);

    let certificate = verify_workspace_cleanup(&workspace, None);

    assert!(certificate.is_complete(), "{:?}", certificate.failures);
    assert!(certificate.failure_summary().is_empty());
    // Network was not verified by this call, so it must not be reported as clean.
    assert_eq!(certificate.network_complete, None);
    assert_eq!(certificate.ports_released, None);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn leftover_runtime_markers_and_a_live_pid_fail_the_certificate() {
    // This test process is a runtime that is definitely still alive.
    let pid = std::process::id();
    let starttime = crate::workspace::session::process_starttime_ticks(pid).unwrap();
    let (workspace, root) = fixture("markers", WorkspaceStatus::Stopping, Some((pid, starttime)));
    fs::write(session::runtime_pid_file(&workspace), "1\n").unwrap();
    fs::write(session::runtime_ready_file(&workspace), "ready\n").unwrap();

    let certificate = verify_workspace_cleanup(&workspace, None);

    assert!(!certificate.is_complete());
    assert!(!certificate.runtime_exited);
    assert!(!certificate.runtime_files_removed);
    let summary = certificate.failure_summary();
    assert!(summary.contains("runtime"), "{summary}");
    assert!(summary.contains("runtime_files"), "{summary}");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn an_incomplete_network_report_fails_the_certificate() {
    let (workspace, root) = fixture("network-incomplete", WorkspaceStatus::Stopped, None);
    let report = NetworkCleanupReport {
        workspace_id: workspace.id.clone(),
        assigned_ip: "10.200.0.9".to_string(),
        veth_host: Some("veth-x".to_string()),
        anti_spoof_rules_absent: true,
        veth_absent: false,
        failures: Vec::new(),
    };

    let certificate = verify_workspace_cleanup(&workspace, Some(&report));

    assert!(!certificate.is_complete());
    assert_eq!(certificate.network_complete, Some(false));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_complete_network_report_keeps_the_certificate_complete() {
    let (workspace, root) = fixture("network-complete", WorkspaceStatus::Stopped, None);
    let report = NetworkCleanupReport {
        workspace_id: workspace.id.clone(),
        assigned_ip: "10.200.0.9".to_string(),
        veth_host: Some("veth-x".to_string()),
        anti_spoof_rules_absent: true,
        veth_absent: true,
        failures: Vec::new(),
    };

    let certificate = verify_workspace_cleanup(&workspace, Some(&report));

    assert!(certificate.is_complete(), "{:?}", certificate.failures);
    assert_eq!(certificate.network_complete, Some(true));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn certificate_serializes_for_operation_reports() {
    let (workspace, root) = fixture("serialize", WorkspaceStatus::Stopped, None);
    let certificate = verify_workspace_cleanup(&workspace, None);

    let rendered = serde_json::to_value(&certificate).expect("serialize certificate");
    assert_eq!(rendered["workspace_id"], "workspace-id");
    assert_eq!(rendered["runtime_exited"], true);
    assert!(rendered["failures"].as_array().unwrap().is_empty());

    let _ = fs::remove_dir_all(root);
}

/// A destroy must prove the workspace directory is gone. A stop keeps it, so this
/// check exists only on the destroy path and it fails closed.
#[test]
fn destroy_verification_reports_a_surviving_workspace_directory() {
    let (workspace, root) = fixture("destroy-files", WorkspaceStatus::Stopped, None);

    let certificate = verify_workspace_destroyed(&workspace, true);

    assert!(!certificate.is_complete());
    assert_eq!(certificate.files_removed, Some(false));
    assert!(certificate
        .failures
        .iter()
        .any(|failure| failure.resource == "files"));

    let _ = fs::remove_dir_all(root);
}

/// The positive case: nothing the record described is found afterwards, so the
/// destroy certificate is complete.
#[test]
fn destroy_verification_is_complete_once_everything_is_gone() {
    let (workspace, root) = fixture("destroy-clean", WorkspaceStatus::Stopped, None);
    fs::remove_dir_all(&workspace.workspace_path).unwrap();

    let certificate = verify_workspace_destroyed(&workspace, true);

    assert!(certificate.is_complete(), "{:?}", certificate.failures);
    assert_eq!(certificate.files_removed, Some(true));

    let _ = fs::remove_dir_all(root);
}

/// A network teardown the caller could not complete has to fail the destroy
/// certificate, so a destroy cannot report the host clean while a veth or rule
/// survives.
#[test]
fn destroy_verification_fails_when_the_network_was_not_released() {
    let (workspace, root) = fixture("destroy-network", WorkspaceStatus::Stopped, None);
    fs::remove_dir_all(&workspace.workspace_path).unwrap();

    let certificate = verify_workspace_destroyed(&workspace, false);

    assert!(!certificate.is_complete());
    assert_eq!(certificate.network_complete, Some(false));

    let _ = fs::remove_dir_all(root);
}

#[allow(dead_code)]
fn unused_sandbox_fixture(sandbox_dir: &std::path::Path) -> SandboxMetadata {
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
        mounted_rootfs_path: sandbox_dir.join("rootfs.mnt").to_string_lossy().to_string(),
        workspaces_path: sandbox_dir.join("workspaces").to_string_lossy().to_string(),
        home_base_path: sandbox_dir.join("home-base").to_string_lossy().to_string(),
        limits: SandboxLimits::default(),
        status: SandboxStatus::Stopped,
    }
}
