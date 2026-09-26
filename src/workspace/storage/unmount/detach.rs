//! Making the unmount call, and naming why it failed.
//!
//! The call itself is one syscall, so almost all of this module is the
//! diagnostic: a busy unmount reports only an errno, and the operator needs the
//! processes and namespaces still holding the mount.

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use anyhow::Result;

pub(crate) fn unmount_workspace_path(path: &Path, owner_is_dead: bool) -> Result<()> {
    match unmount_path(path, 0) {
        Ok(()) => Ok(()),
        Err(error) if is_already_unmounted_errno(error.raw_os_error()) => Ok(()),
        Err(_) if owner_is_dead => {
            crate::perf::record_cleanup_retry();
            match unmount_path(path, libc::MNT_DETACH) {
                Ok(()) => Ok(()),
                Err(error) if is_already_unmounted_errno(error.raw_os_error()) => Ok(()),
                Err(error) => Err(unmount_error(path, &error)),
            }
        }
        Err(error) => Err(unmount_error(path, &error)),
    }
}

pub(super) fn unmount_path(path: &Path, flags: i32) -> std::io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let result = unsafe { libc::umount2(path.as_ptr(), flags) };
    if result == 0 {
        crate::perf::record_unmount();
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub(crate) fn unmount_error(path: &Path, error: &std::io::Error) -> anyhow::Error {
    let holders = mount_holders(path);
    anyhow::anyhow!(
        "failed to unmount workspace mount target={} errno={} namespace_holders={} detail={}",
        path.display(),
        format_errno(error.raw_os_error()),
        if holders.is_empty() {
            "none".to_string()
        } else {
            holders.join(",")
        },
        error
    )
}

pub(super) fn is_already_unmounted_errno(errno: Option<i32>) -> bool {
    matches!(errno, Some(libc::ENOENT | libc::EINVAL))
}

pub(crate) fn mount_holders(path: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
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
        let mountinfo_path = entry.path().join("mountinfo");
        let Ok(mountinfo) = fs::read_to_string(mountinfo_path) else {
            continue;
        };
        if !crate::fsutil::MountInfoSnapshot::parse(&mountinfo).contains(path) {
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
    holders
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
