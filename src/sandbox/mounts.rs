use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use super::types::SandboxMetadata;
use super::util::command_failure_detail;

pub fn ensure_rootfs_mounted(metadata: &SandboxMetadata) -> Result<()> {
    let (rootfs_path, mounted_rootfs_path) = validate_mount_paths(metadata)?;
    if crate::fsutil::is_mountpoint(Path::new(&mounted_rootfs_path))? {
        return Ok(());
    }

    let output = Command::new("mount")
        .arg("--bind")
        .arg(&rootfs_path)
        .arg(&mounted_rootfs_path)
        .output()
        .context("failed to run mount --bind")?;
    if !output.status.success() {
        return Err(anyhow!(
            "failed to mount sandbox rootfs ({}): {}",
            output.status,
            command_failure_detail(&output)
        ));
    }

    let output = Command::new("mount")
        .arg("--make-private")
        .arg(&mounted_rootfs_path)
        .output()
        .context("failed to run mount --make-private")?;
    if !output.status.success() {
        return Err(anyhow!(
            "failed to mark mount private ({}): {}",
            output.status,
            command_failure_detail(&output)
        ));
    }

    Ok(())
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

fn unmount_path(path: &Path) -> std::io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let result = unsafe { libc::umount2(path.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn is_already_unmounted_errno(errno: Option<i32>) -> bool {
    matches!(errno, Some(libc::ENOENT | libc::EINVAL))
}

fn format_errno(errno: Option<i32>) -> String {
    match errno {
        Some(libc::EBUSY) => "EBUSY(16)".to_string(),
        Some(libc::ENOENT) => "ENOENT(2)".to_string(),
        Some(libc::EINVAL) => "EINVAL(22)".to_string(),
        Some(libc::EPERM) => "EPERM(1)".to_string(),
        Some(value) => format!("errno({value})"),
        None => "unknown".to_string(),
    }
}

fn mount_holders(path: &str) -> String {
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

fn validate_unmount_path(metadata: &SandboxMetadata) -> Result<Option<String>> {
    let sandbox_dir = PathBuf::from(&metadata.sandbox_path);
    let sandbox_metadata = match fs::symlink_metadata(&sandbox_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to stat sandbox path {}", sandbox_dir.display()))
        }
    };
    if sandbox_metadata.file_type().is_symlink() {
        bail!(
            "sandbox path {} must not be a symlink",
            sandbox_dir.display()
        );
    }

    let mounted_rootfs_path = PathBuf::from(&metadata.mounted_rootfs_path);
    match fs::symlink_metadata(&mounted_rootfs_path) {
        Ok(path_metadata) if path_metadata.file_type().is_symlink() => {
            bail!(
                "mounted rootfs path {} must not be a symlink",
                mounted_rootfs_path.display()
            )
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to stat mounted rootfs path {}",
                    mounted_rootfs_path.display()
                )
            })
        }
    }
    let canonical_mounted = crate::fsutil::ensure_path_within(
        &sandbox_dir,
        &mounted_rootfs_path,
        "mounted rootfs path",
    )?;

    let rootfs_path = PathBuf::from(&metadata.rootfs_path);
    reject_symlink_if_present("rootfs path", &rootfs_path)?;
    if rootfs_path.exists() {
        let canonical_rootfs =
            crate::fsutil::ensure_path_within(&sandbox_dir, &rootfs_path, "rootfs path")?;
        if canonical_rootfs == canonical_mounted {
            bail!("rootfs and mounted rootfs paths must not be the same");
        }
    }

    Ok(Some(canonical_mounted.to_string_lossy().to_string()))
}

fn reject_symlink_if_present(label: &str, path: &std::path::Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("{} {} must not be a symlink", label, path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to stat {} {}", label, path.display()))
        }
    }
}

fn validate_mount_paths(metadata: &SandboxMetadata) -> Result<(String, String)> {
    let sandbox_dir = PathBuf::from(&metadata.sandbox_path);
    let rootfs_path = PathBuf::from(&metadata.rootfs_path);
    let mounted_rootfs_path = PathBuf::from(&metadata.mounted_rootfs_path);

    for (label, path) in [
        ("sandbox path", &sandbox_dir),
        ("rootfs path", &rootfs_path),
        ("mounted rootfs path", &mounted_rootfs_path),
    ] {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("failed to stat {} {}", label, path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("{} {} must not be a symlink", label, path.display());
        }
    }

    let canonical_rootfs =
        crate::fsutil::canonicalize_within(&sandbox_dir, &rootfs_path, "rootfs path")?;
    let canonical_mounted = crate::fsutil::canonicalize_within(
        &sandbox_dir,
        &mounted_rootfs_path,
        "mounted rootfs path",
    )?;
    if canonical_rootfs == canonical_mounted {
        bail!("rootfs and mounted rootfs paths must not be the same");
    }

    Ok((
        canonical_rootfs.to_string_lossy().to_string(),
        canonical_mounted.to_string_lossy().to_string(),
    ))
}

#[cfg(test)]
#[path = "../../tests/src/sandbox/mounts.rs"]
mod tests;
