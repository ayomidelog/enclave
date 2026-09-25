//! Detaching a sandbox mount, and describing why a detach failed.

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use anyhow::{anyhow, bail, Result};

use super::super::types::SandboxMetadata;
use super::paths::validate_unmount_path;

pub(super) fn unmount_path(path: &Path) -> std::io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let result = unsafe { libc::umount2(path.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub(crate) fn is_already_unmounted_errno(errno: Option<i32>) -> bool {
    matches!(errno, Some(libc::ENOENT | libc::EINVAL))
}

pub(super) fn format_errno(errno: Option<i32>) -> String {
    match errno {
        Some(libc::EBUSY) => "EBUSY(16)".to_string(),
        Some(libc::ENOENT) => "ENOENT(2)".to_string(),
        Some(libc::EINVAL) => "EINVAL(22)".to_string(),
        Some(libc::EPERM) => "EPERM(1)".to_string(),
        Some(value) => format!("errno({value})"),
        None => "unknown".to_string(),
    }
}

/// Which processes still mount `path`, so a busy unmount names a holder rather
/// than only an errno.
pub(super) fn mount_holders(path: &str) -> String {
    let Ok(entries) = fs::read_dir("/proc") else {
        return "none".to_string();
    };
    let mut holders = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|value| value.chars().all(|ch| ch.is_ascii_digit()))
        else {
            continue;
        };
        let Ok(mountinfo) = fs::read_to_string(entry.path().join("mountinfo")) else {
            continue;
        };
        let owns_mount = mountinfo
            .lines()
            .any(|line| line.split_whitespace().nth(4) == Some(path));
        if !owns_mount {
            continue;
        }
        let namespace = fs::read_link(entry.path().join("ns/mnt"))
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        holders.push(format!("pid={pid}@{namespace}"));
        if holders.len() == 8 {
            break;
        }
    }
    if holders.is_empty() {
        "none".to_string()
    } else {
        holders.join(",")
    }
}

pub fn ensure_rootfs_unmounted(metadata: &SandboxMetadata) -> Result<()> {
    let Some(mounted_rootfs_path) = validate_unmount_path(metadata)? else {
        return Ok(());
    };
    if !crate::fsutil::is_mountpoint(Path::new(&mounted_rootfs_path))? {
        return Ok(());
    }

    if let Err(error) = unmount_path(Path::new(&mounted_rootfs_path)) {
        if is_already_unmounted_errno(error.raw_os_error()) {
            return Ok(());
        }
        return Err(anyhow!(
            "failed to unmount sandbox rootfs mount_target={} errno={} namespace_holders={} detail={}",
            mounted_rootfs_path,
            format_errno(error.raw_os_error()),
            mount_holders(&mounted_rootfs_path),
            error
        ));
    }
    if crate::fsutil::is_mountpoint(Path::new(&mounted_rootfs_path))? {
        bail!(
            "sandbox rootfs mount {} is still present after umount",
            mounted_rootfs_path
        );
    }
    Ok(())
}
