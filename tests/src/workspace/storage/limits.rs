//! What a workspace is allowed to ask for, and which storage it gets.

use super::*;

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

#[test]
fn root_overlay_paths_use_quota_filesystem_for_upper_and_work() {
    let mut workspace = workspace_fixture();
    workspace.limits.disk_bytes = Some(MIN_DISK_BYTES);
    let (upper, work, merged) = root_overlay_paths(&workspace).expect("quota root overlay");
    assert_eq!(
        upper,
        std::path::PathBuf::from("/tmp/enclave-test/workspaces/ws-123/fs/root-upper")
    );
    assert_eq!(
        work,
        std::path::PathBuf::from("/tmp/enclave-test/workspaces/ws-123/fs/root-work")
    );
    assert_eq!(
        merged,
        std::path::PathBuf::from("/tmp/enclave-test/workspaces/ws-123/root-merged")
    );
}
