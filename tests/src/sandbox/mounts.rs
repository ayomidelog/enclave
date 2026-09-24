use super::*;
use std::fs;

use crate::sandbox::{BootstrapMethod, SandboxLimits, SandboxStatus};

fn metadata_for_paths(root: &std::path::Path) -> SandboxMetadata {
    SandboxMetadata {
        id: "sandbox-id".to_string(),
        name: "sandbox".to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: BootstrapMethod::CachedRootfs,
        created_at: "2026-08-06T00:00:00Z".to_string(),
        sandbox_path: root.to_string_lossy().to_string(),
        rootfs_path: root.join("rootfs").to_string_lossy().to_string(),
        rootfs_lower_path: None,
        mounted_rootfs_path: root
            .join("runtime")
            .join("rootfs.mnt")
            .to_string_lossy()
            .to_string(),
        workspaces_path: root.join("workspaces").to_string_lossy().to_string(),
        home_base_path: root.join("home-base").to_string_lossy().to_string(),
        limits: SandboxLimits::default(),
        status: SandboxStatus::Stopped,
    }
}

/// A shared base needs its own writable upper and work directories, both
/// siblings of the rootfs mount point.
#[test]
fn overlay_paths_are_siblings_of_the_rootfs_mount() {
    let root = std::path::Path::new("/var/lib/enclave/sandboxes/sb");
    let mut metadata = metadata_for_paths(root);
    assert!(rootfs_overlay_paths(&metadata).is_none());

    metadata.rootfs_lower_path = Some("/var/lib/enclave/rootfs-cache/bookworm".to_string());
    let (upper, work) = rootfs_overlay_paths(&metadata).expect("shared base overlay paths");
    assert_eq!(upper, root.join("rootfs-upper"));
    assert_eq!(work, root.join("rootfs-work"));
    assert_eq!(upper.parent(), work.parent());
}

/// OverlayFS splits its options on `,` and `:`, so a base path containing
/// either would silently corrupt the mount options.
#[test]
fn overlay_option_paths_escape_separators() {
    assert_eq!(
        overlay_option_path(std::path::Path::new("/a,b")),
        "/a\\054b"
    );
    assert_eq!(
        overlay_option_path(std::path::Path::new("/a:b")),
        "/a\\072b"
    );
    assert_eq!(
        overlay_option_path(std::path::Path::new("/plain/path")),
        "/plain/path"
    );
}

/// A sandbox without a shared base must not attempt an overlay mount, so the
/// plain rootfs directory is left exactly as the copy left it.
#[test]
fn overlay_mount_is_a_noop_without_a_shared_base() {
    let root = std::env::temp_dir().join(format!(
        "enclave-mounts-no-base-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let metadata = metadata_for_paths(&root);
    ensure_rootfs_overlay_mounted(&metadata).expect("no-op overlay mount");
    unmount_rootfs_overlay(&metadata).expect("no-op overlay unmount");
    assert!(!root.exists());
}

/// A missing shared base must be reported instead of mounting an empty rootfs.
#[test]
fn overlay_mount_rejects_a_missing_shared_base() {
    let root = std::env::temp_dir().join(format!(
        "enclave-mounts-missing-base-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut metadata = metadata_for_paths(&root);
    metadata.rootfs_lower_path = Some(root.join("does-not-exist").to_string_lossy().to_string());
    let error = ensure_rootfs_overlay_mounted(&metadata).expect_err("missing base must fail");
    assert!(
        error.to_string().contains("is missing"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn unmount_is_ok_when_rootfs_and_mount_paths_are_missing() {
    let temp_dir =
        std::env::temp_dir().join(format!("enclave-mounts-missing-{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(temp_dir.join("runtime")).unwrap();

    let metadata = metadata_for_paths(&temp_dir);
    ensure_rootfs_unmounted(&metadata).unwrap();

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn unmount_is_ok_when_mount_path_is_missing() {
    let temp_dir =
        std::env::temp_dir().join(format!("enclave-mounts-no-mount-{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(temp_dir.join("rootfs")).unwrap();
    fs::create_dir_all(temp_dir.join("runtime")).unwrap();

    let metadata = metadata_for_paths(&temp_dir);
    ensure_rootfs_unmounted(&metadata).unwrap();

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn unmount_rejects_symlinked_mount_path() {
    let temp_dir =
        std::env::temp_dir().join(format!("enclave-mounts-symlink-{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(temp_dir.join("rootfs")).unwrap();
    fs::create_dir_all(temp_dir.join("runtime")).unwrap();
    std::os::unix::fs::symlink("/tmp", temp_dir.join("runtime").join("rootfs.mnt")).unwrap();

    let metadata = metadata_for_paths(&temp_dir);
    let error = ensure_rootfs_unmounted(&metadata).unwrap_err();
    assert!(error.to_string().contains("must not be a symlink"));

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn unmount_errno_classifier_accepts_missing_state_errors() {
    assert!(is_already_unmounted_errno(Some(libc::ENOENT)));
    assert!(is_already_unmounted_errno(Some(libc::EINVAL)));
    assert!(!is_already_unmounted_errno(Some(libc::EBUSY)));
}
