//! Growing a workspace image, and the requests that are refused.

use super::*;

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
    assert!(
        super::ext4::filesystem_size(&image).expect("read ext4 size") >= expanded_bytes,
        "the ext4 filesystem must grow with the image"
    );
    let _ = std::fs::remove_dir_all(root);
}
