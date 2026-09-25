//! The workspace private /tmp.
//!
//! Resetting it must empty the directory without removing it, because the mountpoint
//! it is bound onto has to keep existing.

use super::*;

#[test]
fn reset_workspace_tmp_removes_contents_but_keeps_directory() {
    let root = std::env::temp_dir().join(format!("enclave-tmp-reset-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let tmp = root
        .join("workspace/fs")
        .join(super::super::WORKSPACE_TMP_DIR);
    std::fs::create_dir_all(tmp.join("nested")).expect("create tmp fixture");
    std::fs::write(tmp.join("file"), "data").expect("write tmp fixture");

    clear_workspace_tmp_contents(&root.join("workspace/fs")).expect("reset workspace tmp");

    assert!(tmp.is_dir());
    assert!(!tmp.join("file").exists());
    assert!(!tmp.join("nested").exists());
    let _ = std::fs::remove_dir_all(root);
}

/// A workspace created before the `/tmp` backing directory was hidden keeps its
/// `/tmp` content in a plain `tmp` directory that the workspace can also see as
/// `/home/tmp`. The layout step has to move that content once, without leaving
/// either directory missing or duplicated.
#[test]
fn ensure_workspace_tmp_layout_moves_the_legacy_directory() {
    let root = std::env::temp_dir().join(format!("enclave-tmp-layout-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let filesystem = root.join("workspace/fs");
    std::fs::create_dir_all(filesystem.join(crate::workspace::LEGACY_WORKSPACE_TMP_DIR))
        .expect("create legacy tmp fixture");
    std::fs::write(
        filesystem
            .join(crate::workspace::LEGACY_WORKSPACE_TMP_DIR)
            .join("file"),
        "kept",
    )
    .expect("write legacy tmp file");

    let mut workspace = workspace_fixture();
    workspace.workspace_path = root.join("workspace").to_string_lossy().to_string();
    workspace.filesystem_path = filesystem.to_string_lossy().to_string();
    workspace.limits.disk_bytes = Some(MIN_DISK_BYTES);

    ensure_workspace_tmp_layout(&workspace).expect("ensure tmp layout");

    let hidden = filesystem.join(super::super::WORKSPACE_TMP_DIR);
    assert!(hidden.is_dir());
    assert_eq!(
        std::fs::read_to_string(hidden.join("file")).expect("read moved file"),
        "kept"
    );
    assert!(!filesystem
        .join(crate::workspace::LEGACY_WORKSPACE_TMP_DIR)
        .exists());
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&hidden).unwrap().permissions())
            & 0o1777,
        0o1777
    );

    // Running the layout step again is a no-op that keeps the content.
    ensure_workspace_tmp_layout(&workspace).expect("ensure tmp layout twice");
    assert_eq!(
        std::fs::read_to_string(hidden.join("file")).expect("read file after second pass"),
        "kept"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// `/tmp` lives inside the workspace disk image. Resetting it when the image is
/// not mounted must fail loudly instead of clearing the host mountpoint and
/// reporting success while the image keeps its contents.
#[test]
fn reset_workspace_tmp_never_succeeds_without_mounted_storage() {
    let root = std::env::temp_dir().join(format!("enclave-tmp-unmounted-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut workspace = workspace_fixture();
    workspace.workspace_path = root.join("workspace").to_string_lossy().to_string();
    workspace.filesystem_path = root.join("workspace/fs").to_string_lossy().to_string();
    workspace.limits.disk_bytes = Some(MIN_DISK_BYTES);
    std::fs::create_dir_all(root.join("workspace/fs/tmp")).expect("create tmp fixture");
    std::fs::write(root.join("workspace/fs/tmp/file"), "data").expect("write tmp fixture");

    let error = reset_mounted_workspace_tmp(&workspace)
        .expect_err("an unmounted workspace filesystem must be refused");
    assert!(
        error.to_string().contains("not mounted"),
        "unexpected error: {error:#}"
    );
    assert!(root.join("workspace/fs/tmp/file").exists());
    let _ = std::fs::remove_dir_all(root);
}
