use super::*;

fn workspace_fixture() -> super::super::types::WorkspaceMetadata {
    super::super::types::WorkspaceMetadata {
        id: "ws-123".to_string(),
        sandbox_id: "sb-123".to_string(),
        name: "dev".to_string(),
        created_at: "2026-03-11T00:00:00Z".to_string(),
        workspace_path: "/tmp/enclave-test/workspaces/ws-123".to_string(),
        filesystem_path: "/tmp/enclave-test/workspaces/ws-123/fs".to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: "/tmp/enclave-test/rootfs".to_string(),
        overlay_home_base_path: "/tmp/enclave-test/home-base".to_string(),
        overlay_home_upper_path: "/tmp/enclave-test/workspaces/ws-123/home-upper".to_string(),
        overlay_home_work_path: "/tmp/enclave-test/workspaces/ws-123/home-work".to_string(),
        overlay_home_merged_path: "/tmp/enclave-test/workspaces/ws-123/home-merged".to_string(),
        auth_providers: Vec::new(),
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: Default::default(),
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: Default::default(),
        limits: Default::default(),
        assigned_ip: None,
    }
}

#[test]
fn validate_workspace_storage_limits_accepts_absent_disk_quota() {
    validate_workspace_storage_limits(None, None).expect("no quota should be accepted");
}

#[test]
fn validate_workspace_storage_limits_rejects_small_disk_quota() {
    let err = validate_workspace_storage_limits(None, Some(MIN_DISK_BYTES - 1))
        .expect_err("small quota must fail");
    assert!(err.to_string().contains("disk quota"));
}

#[test]
fn validate_workspace_storage_limits_rejects_host_mount_with_quota() {
    let err = validate_workspace_storage_limits(Some("/host/project"), Some(MIN_DISK_BYTES))
        .expect_err("host mounts should not support quotas");
    assert!(err.to_string().contains("workspace_dir/path host mounts"));
}

#[test]
fn workspace_uses_disk_image_only_for_managed_workspace_with_quota() {
    let mut workspace = workspace_fixture();
    assert!(!workspace_uses_disk_image(&workspace));

    workspace.limits.disk_bytes = Some(MIN_DISK_BYTES);
    assert!(workspace_uses_disk_image(&workspace));

    workspace.home_mount_source_path = Some("/host/project".to_string());
    assert!(!workspace_uses_disk_image(&workspace));
}

#[test]
fn workspace_disk_image_path_is_under_workspace_dir() {
    let workspace = workspace_fixture();
    assert_eq!(
        workspace_disk_image_path(&workspace),
        std::path::PathBuf::from("/tmp/enclave-test/workspaces/ws-123/fs.img")
    );
}
