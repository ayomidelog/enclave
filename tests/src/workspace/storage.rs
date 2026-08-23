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
        clear_tmp_on_restart: false,
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

#[test]
fn reset_workspace_tmp_removes_contents_but_keeps_directory() {
    let root = std::env::temp_dir().join(format!("enclave-tmp-reset-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut workspace = workspace_fixture();
    workspace.workspace_path = root.join("workspace").to_string_lossy().to_string();
    workspace.filesystem_path = root.join("workspace/fs").to_string_lossy().to_string();
    workspace.limits.disk_bytes = Some(MIN_DISK_BYTES);
    std::fs::create_dir_all(root.join("workspace/fs/tmp/nested")).expect("create tmp fixture");
    std::fs::write(root.join("workspace/fs/tmp/file"), "data").expect("write tmp fixture");

    reset_workspace_tmp(&workspace).expect("reset workspace tmp");

    assert!(root.join("workspace/fs/tmp").is_dir());
    assert!(!root.join("workspace/fs/tmp/file").exists());
    assert!(!root.join("workspace/fs/tmp/nested").exists());
    let _ = std::fs::remove_dir_all(root);
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

#[test]
fn mountinfo_parser_preserves_nested_mounts_for_reverse_cleanup() {
    let mountinfo = concat!(
        "100 1 0:50 / /tmp/enclave/ws/fs rw,relatime - ext4 /dev/loop0 rw\n",
        "101 100 0:51 / /tmp/enclave/ws/fs/cache\\040data rw,relatime - tmpfs tmpfs rw\n"
    );
    let mut mountpoints = parse_mountinfo_mountpoints(mountinfo)
        .into_iter()
        .filter(|path| path.starts_with("/tmp/enclave/ws/fs"))
        .collect::<Vec<_>>();
    mountpoints.sort_by_key(|path| std::cmp::Reverse(path.components().count()));

    assert_eq!(
        mountpoints,
        vec![
            PathBuf::from("/tmp/enclave/ws/fs/cache data"),
            PathBuf::from("/tmp/enclave/ws/fs"),
        ]
    );
}

#[test]
fn dead_workspace_owner_allows_lazy_unmount_fallback() {
    let mut workspace = workspace_fixture();
    workspace.runtime_pid = Some(u32::MAX);
    workspace.runtime_starttime_ticks = Some(1);
    assert!(workspace_owner_is_dead(&workspace));
}

#[test]
fn missing_starttime_does_not_block_lazy_unmount_fallback() {
    let mut workspace = workspace_fixture();
    workspace.runtime_pid = Some(std::process::id());
    workspace.runtime_starttime_ticks = None;
    assert!(workspace_owner_is_dead(&workspace));
}
