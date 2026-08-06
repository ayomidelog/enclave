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

#[test]
fn disk_resize_rejects_workspace_without_allocation() {
    let workspace = workspace_fixture();
    let err = increase_workspace_disk_allocation(&workspace, MIN_DISK_BYTES * 2)
        .expect_err("workspace without a disk allocation must fail");
    assert!(err
        .to_string()
        .contains("no Enclave-managed disk allocation"));
}

#[test]
fn disk_resize_rejects_host_backed_workspace() {
    let mut workspace = workspace_fixture();
    workspace.limits.disk_bytes = Some(MIN_DISK_BYTES);
    workspace.home_mount_source_path = Some("/host/project".to_string());
    let err = increase_workspace_disk_allocation(&workspace, MIN_DISK_BYTES * 2)
        .expect_err("host-backed workspace must fail");
    assert!(err.to_string().contains("host-backed workspace directory"));
}

#[test]
fn disk_resize_rejects_decrease() {
    let mut workspace = workspace_fixture();
    workspace.limits.disk_bytes = Some(MIN_DISK_BYTES * 2);
    let err = increase_workspace_disk_allocation(&workspace, MIN_DISK_BYTES)
        .expect_err("decreases must fail");
    assert!(err.to_string().contains("only supports increases"));
}

#[test]
fn disk_resize_equal_size_is_a_noop() {
    let mut workspace = workspace_fixture();
    workspace.limits.disk_bytes = Some(MIN_DISK_BYTES);
    let result = increase_workspace_disk_allocation(&workspace, MIN_DISK_BYTES)
        .expect("equal allocation should be a no-op");
    assert_eq!(result.previous_bytes, MIN_DISK_BYTES);
    assert_eq!(result.new_bytes, MIN_DISK_BYTES);
}

#[test]
fn disk_resize_grows_real_ext4_image() {
    let root = std::env::temp_dir().join(format!(
        "enclave-storage-resize-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create resize test directory");

    let initial_bytes = MIN_DISK_BYTES;
    let expanded_bytes = MIN_DISK_BYTES * 2;
    let image = root.join("fs.img");
    let mountpoint = root.join("fs");
    let truncate = std::process::Command::new("truncate")
        .args(["-s", &initial_bytes.to_string()])
        .arg(&image)
        .status()
        .expect("run truncate");
    assert!(truncate.success());
    let mkfs = std::process::Command::new("mkfs.ext4")
        .args(["-F", "-q"])
        .arg(&image)
        .status()
        .expect("run mkfs.ext4");
    assert!(mkfs.success());

    let mut workspace = workspace_fixture();
    workspace.workspace_path = root.to_string_lossy().into_owned();
    workspace.filesystem_path = mountpoint.to_string_lossy().into_owned();
    workspace.limits.disk_bytes = Some(initial_bytes);
    let result =
        increase_workspace_disk_allocation(&workspace, expanded_bytes).expect("grow ext4 image");

    assert_eq!(result.previous_bytes, initial_bytes);
    assert_eq!(result.new_bytes, expanded_bytes);
    assert_eq!(
        std::fs::metadata(&image).expect("image metadata").len(),
        expanded_bytes
    );
    let _ = std::fs::remove_dir_all(root);
}
