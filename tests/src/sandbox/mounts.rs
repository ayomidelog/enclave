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
fn unmount_error_classifier_accepts_missing_state_errors() {
    assert!(is_already_unmounted_error(
        "umount: /path: No such file or directory"
    ));
    assert!(is_already_unmounted_error(
        "umount: /path: Invalid argument"
    ));
    assert!(is_already_unmounted_error("umount: /path: not mounted"));
    assert!(!is_already_unmounted_error("umount: /path: target is busy"));
}
