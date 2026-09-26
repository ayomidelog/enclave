//! Resizing a workspace image in either direction, and the requests that are refused.

use std::process::Command;

use super::*;
use crate::workspace::storage::{plan_workspace_disk_resize, workspace_disk_image_path};

/// Whether a host tool the test needs is installed.
///
/// The tests that build a real image are skipped rather than failed when one is
/// missing, so a host without e2fsprogs still runs the rest of the suite.
fn tool_is_available(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|status| status.success())
}

/// A workspace whose image is a real ext4 filesystem of `bytes`.
///
/// The image is created the way Enclave creates one, so the resize runs against the
/// same layout it does in production rather than a stand-in.
fn ext4_image(root: &std::path::Path, bytes: u64, mkfs_extra: &[&str]) -> std::path::PathBuf {
    std::fs::create_dir_all(root).expect("create resize test directory");
    let image = root.join("fs.img");
    assert!(Command::new("truncate")
        .args(["-s", &bytes.to_string()])
        .arg(&image)
        .status()
        .expect("run truncate")
        .success());
    let mut mkfs = Command::new("mkfs.ext4");
    mkfs.args(["-F", "-q"]).args(mkfs_extra).arg(&image);
    assert!(mkfs.status().expect("run mkfs.ext4").success());
    image
}

fn resizable_workspace(
    root: &std::path::Path,
    image: &std::path::Path,
    bytes: u64,
) -> WorkspaceMetadata {
    let mut workspace = workspace_fixture();
    workspace.workspace_path = root.to_string_lossy().into_owned();
    workspace.filesystem_path = root.join("fs").to_string_lossy().into_owned();
    workspace.limits.disk_bytes = Some(bytes);
    // The resize finds the image from the record, so the fixture's record has to
    // name the file the test built.
    assert_eq!(workspace_disk_image_path(&workspace), image);
    workspace
}

/// A directory of its own per test, so two tests never share an image path.
fn test_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("enclave-resize-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

#[test]
fn disk_resize_rejects_workspace_without_allocation() {
    let workspace = workspace_fixture();
    let err = resize_workspace_disk_allocation(&workspace, MIN_DISK_BYTES * 2)
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
    let err = resize_workspace_disk_allocation(&workspace, MIN_DISK_BYTES * 2)
        .expect_err("host-backed workspace must fail");
    assert!(err.to_string().contains("host-backed workspace directory"));
}

#[test]
fn disk_resize_rejects_an_allocation_below_the_floor() {
    let mut workspace = workspace_fixture();
    workspace.limits.disk_bytes = Some(MIN_DISK_BYTES * 4);
    let err = resize_workspace_disk_allocation(&workspace, MIN_DISK_BYTES - 1)
        .expect_err("an allocation below the floor must fail");
    assert!(err.to_string().contains("at least"), "{err}");
}

#[test]
fn disk_resize_equal_size_is_a_noop() {
    let mut workspace = workspace_fixture();
    workspace.limits.disk_bytes = Some(MIN_DISK_BYTES);
    let result = resize_workspace_disk_allocation(&workspace, MIN_DISK_BYTES)
        .expect("equal allocation should be a no-op");
    assert_eq!(result.previous_bytes, MIN_DISK_BYTES);
    assert_eq!(result.new_bytes, MIN_DISK_BYTES);
}

#[test]
fn disk_resize_grows_real_ext4_image() {
    if !tool_is_available("truncate") || !tool_is_available("mkfs.ext4") {
        return;
    }
    let root = test_root("grow");
    let initial_bytes = MIN_DISK_BYTES;
    let expanded_bytes = MIN_DISK_BYTES * 2;
    let image = ext4_image(&root, initial_bytes, &[]);
    let workspace = resizable_workspace(&root, &image, initial_bytes);

    let result =
        resize_workspace_disk_allocation(&workspace, expanded_bytes).expect("grow ext4 image");

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

/// The image file and the filesystem inside it have to end up the same size.
///
/// A filesystem smaller than its image is not an error, but it is a difference an
/// operator cannot see and that the next resize has to cope with. The shrink is
/// expressed in whole blocks so the two land on the same number.
#[test]
fn disk_resize_shrinks_real_ext4_image() {
    if !tool_is_available("truncate") || !tool_is_available("mkfs.ext4") {
        return;
    }
    let root = test_root("shrink");
    let initial_bytes = MIN_DISK_BYTES * 4;
    let shrunk_bytes = MIN_DISK_BYTES * 2;
    let image = ext4_image(&root, initial_bytes, &[]);
    let workspace = resizable_workspace(&root, &image, initial_bytes);

    let result =
        resize_workspace_disk_allocation(&workspace, shrunk_bytes).expect("shrink ext4 image");

    assert_eq!(result.previous_bytes, initial_bytes);
    assert_eq!(result.new_bytes, shrunk_bytes);
    assert_eq!(
        std::fs::metadata(&image).expect("image metadata").len(),
        shrunk_bytes
    );
    assert_eq!(
        super::ext4::filesystem_size(&image).expect("read ext4 size"),
        shrunk_bytes,
        "the filesystem must shrink with the image"
    );
    // And the result is still a filesystem, which a cut-off one would not be.
    let check = Command::new("e2fsck")
        .args(["-f", "-n"])
        .arg(&image)
        .status();
    if let Ok(status) = check {
        assert!(
            status.success(),
            "the shrunk filesystem must still check clean"
        );
    }
    let _ = std::fs::remove_dir_all(root);
}

/// A shrink the data does not fit in is refused before anything is written.
///
/// The floor is read from `resize2fs -P`, so the message names the smallest
/// allocation that would work instead of a tool refusal the operator has to decode.
#[test]
fn disk_resize_refuses_a_shrink_the_filesystem_does_not_fit() {
    if !tool_is_available("truncate") || !tool_is_available("mkfs.ext4") {
        return;
    }
    let root = test_root("floor");
    let initial_bytes = MIN_DISK_BYTES * 4;
    // A filesystem with far more inodes than it needs has a high floor: the inode
    // tables alone do not fit in the allocation this test asks for.
    let image = ext4_image(&root, initial_bytes, &["-N", "300000"]);
    let workspace = resizable_workspace(&root, &image, initial_bytes);

    let error = resize_workspace_disk_allocation(&workspace, MIN_DISK_BYTES)
        .expect_err("a shrink below the filesystem's floor must fail");
    let rendered = format!("{error:#}");
    assert!(rendered.contains("holds too much data"), "{rendered}");
    assert!(rendered.contains("MiB"), "{rendered}");
    // Nothing was written: the image and the filesystem are still their old size.
    assert_eq!(
        std::fs::metadata(&image).expect("image metadata").len(),
        initial_bytes
    );
    assert_eq!(
        super::ext4::filesystem_size(&image).expect("read ext4 size"),
        initial_bytes
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A request that cannot be performed is refused before anything is touched.
///
/// This is the property that keeps a refused resize from taking a running workspace
/// down: the caller checks the plan, and only a plan it accepts leads to a stop.
#[test]
fn a_plan_refuses_what_a_resize_would_refuse_without_touching_the_image() {
    if !tool_is_available("truncate") || !tool_is_available("mkfs.ext4") {
        return;
    }
    let root = test_root("plan");
    let initial_bytes = MIN_DISK_BYTES * 2;
    let image = ext4_image(&root, initial_bytes, &[]);
    let workspace = resizable_workspace(&root, &image, initial_bytes);

    // Below the floor.
    let error = plan_workspace_disk_resize(&workspace, MIN_DISK_BYTES - 1)
        .expect_err("a plan below the floor must be refused");
    assert!(error.to_string().contains("at least"), "{error}");

    // A workspace with no managed allocation.
    let mut unmanaged = workspace.clone();
    unmanaged.limits.disk_bytes = None;
    assert!(plan_workspace_disk_resize(&unmanaged, initial_bytes).is_err());

    // A host-backed workspace.
    let mut host_backed = workspace.clone();
    host_backed.home_mount_source_path = Some("/host/project".to_string());
    assert!(plan_workspace_disk_resize(&host_backed, initial_bytes * 2).is_err());

    // Nothing above wrote anything.
    assert_eq!(
        std::fs::metadata(&image).expect("image metadata").len(),
        initial_bytes
    );
    assert_eq!(
        super::ext4::filesystem_size(&image).expect("read ext4 size"),
        initial_bytes
    );

    // And an accepted plan reports the sizes the resize will move between.
    let plan = plan_workspace_disk_resize(&workspace, initial_bytes * 2).expect("plan a grow");
    assert!(plan.is_change());
    assert_eq!(plan.previous_bytes, initial_bytes);
    assert_eq!(plan.new_bytes, initial_bytes * 2);

    // An equal size is a plan that changes nothing, which is what the caller uses to
    // report a no-op rather than to stop the workspace for nothing.
    let same = plan_workspace_disk_resize(&workspace, initial_bytes).expect("plan a no-op");
    assert!(!same.is_change());

    let _ = std::fs::remove_dir_all(root);
}
