use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::types::WorkspaceMetadata;

const DISK_IMAGE_NAME: &str = "fs.img";
const MIN_DISK_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceDiskResize {
    pub previous_bytes: u64,
    pub new_bytes: u64,
}

pub fn validate_workspace_storage_limits(
    home_mount_source_path: Option<&str>,
    disk_bytes: Option<u64>,
) -> Result<()> {
    if let Some(bytes) = disk_bytes {
        if bytes < MIN_DISK_BYTES {
            bail!(
                "workspace disk quota must be at least {} MiB",
                MIN_DISK_BYTES / (1024 * 1024)
            );
        }
        if home_mount_source_path.is_some() {
            bail!(
                "workspace disk quota is not supported with workspace_dir/path host mounts; use the default Enclave-managed workspace storage"
            );
        }
    }
    Ok(())
}

pub fn create_workspace_storage(workspace: &WorkspaceMetadata) -> Result<()> {
    if workspace_uses_disk_image(workspace) {
        ensure_disk_backend_available()?;
        initialize_disk_image(workspace)?;
    }
    Ok(())
}

pub fn ensure_workspace_storage_ready(workspace: &WorkspaceMetadata) -> Result<()> {
    if workspace_uses_disk_image(workspace) {
        ensure_disk_backend_available()?;
        initialize_disk_image(workspace)?;
        mount_disk_image_if_needed(workspace)?;
    }
    Ok(())
}

pub fn ensure_workspace_storage_unmounted(workspace: &WorkspaceMetadata) -> Result<()> {
    let workspace_root = Path::new(&workspace.workspace_path);
    let mut mountpoints = mountpoints_at_or_below(workspace_root)?;
    if mountpoints.is_empty() && is_mountpoint(workspace_root)? {
        mountpoints.push(workspace_root.to_path_buf());
    }

    let owner_is_dead = workspace_owner_is_dead(workspace);
    for path in mountpoints {
        unmount_workspace_path(&path, owner_is_dead)?;
    }
    Ok(())
}

pub(crate) fn unmount_mounts_at_or_below_excluding(
    root: &Path,
    excluded_roots: &[PathBuf],
) -> Result<usize> {
    let mountpoints = mountpoints_at_or_below(root)?;
    let mut unmounted = 0usize;
    for path in mountpoints {
        if excluded_roots
            .iter()
            .any(|excluded| path.starts_with(excluded))
        {
            continue;
        }
        unmount_workspace_path(&path, true)?;
        unmounted += 1;
    }
    Ok(unmounted)
}

fn workspace_owner_is_dead(workspace: &WorkspaceMetadata) -> bool {
    !workspace
        .runtime_pid
        .zip(workspace.runtime_starttime_ticks)
        .is_some_and(|(pid, starttime)| {
            crate::workspace::session_process_matches(pid, Some(starttime))
        })
}

fn unmount_workspace_path(path: &Path, owner_is_dead: bool) -> Result<()> {
    match unmount_path(path, 0) {
        Ok(()) => Ok(()),
        Err(error) if is_already_unmounted_errno(error.raw_os_error()) => Ok(()),
        Err(_) if owner_is_dead => match unmount_path(path, libc::MNT_DETACH) {
            Ok(()) => Ok(()),
            Err(error) if is_already_unmounted_errno(error.raw_os_error()) => Ok(()),
            Err(error) => Err(unmount_error(path, &error)),
        },
        Err(error) => Err(unmount_error(path, &error)),
    }
}

fn unmount_path(path: &Path, flags: i32) -> std::io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let result = unsafe { libc::umount2(path.as_ptr(), flags) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn unmount_error(path: &Path, error: &std::io::Error) -> anyhow::Error {
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

fn is_already_unmounted_errno(errno: Option<i32>) -> bool {
    matches!(errno, Some(libc::ENOENT | libc::EINVAL))
}

fn mount_holders(path: &Path) -> Vec<String> {
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
        if !parse_mountinfo_mountpoints(&mountinfo)
            .iter()
            .any(|mount| mount == path)
        {
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

fn mountpoints_at_or_below(root: &Path) -> Result<Vec<PathBuf>> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")
        .context("failed to read /proc/self/mountinfo")?;
    let mut mountpoints = parse_mountinfo_mountpoints(&mountinfo)
        .into_iter()
        .filter(|mountpoint| mountpoint == root || mountpoint.starts_with(root))
        .collect::<Vec<_>>();
    mountpoints.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    mountpoints.dedup();
    Ok(mountpoints)
}

fn parse_mountinfo_mountpoints(mountinfo: &str) -> Vec<PathBuf> {
    mountinfo
        .lines()
        .filter_map(|line| line.split_whitespace().nth(4))
        .map(unescape_mountinfo_path)
        .map(PathBuf::from)
        .collect()
}

fn unescape_mountinfo_path(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..=index + 3].iter().all(u8::is_ascii_digit)
        {
            let value = (bytes[index + 1] - b'0') * 64
                + (bytes[index + 2] - b'0') * 8
                + (bytes[index + 3] - b'0');
            result.push(value as char);
            index += 4;
        } else {
            result.push(bytes[index] as char);
            index += 1;
        }
    }
    result
}

pub fn increase_workspace_disk_allocation(
    workspace: &WorkspaceMetadata,
    new_disk_bytes: u64,
) -> Result<WorkspaceDiskResize> {
    let current_disk_bytes = workspace.limits.disk_bytes.ok_or_else(|| {
        anyhow::anyhow!(
            "workspace '{}' has no Enclave-managed disk allocation",
            workspace.id
        )
    })?;

    if workspace.home_mount_source_path.is_some() {
        bail!(
            "workspace '{}' uses a host-backed workspace directory; disk allocation resize is only supported for Enclave-managed storage",
            workspace.id
        );
    }
    if new_disk_bytes < MIN_DISK_BYTES {
        bail!(
            "workspace disk allocation must be at least {} MiB",
            MIN_DISK_BYTES / (1024 * 1024)
        );
    }
    if new_disk_bytes < current_disk_bytes {
        bail!(
            "workspace disk resize only supports increases; requested {} bytes is below the current {} bytes",
            new_disk_bytes,
            current_disk_bytes
        );
    }
    if new_disk_bytes == current_disk_bytes {
        return Ok(WorkspaceDiskResize {
            previous_bytes: current_disk_bytes,
            new_bytes: current_disk_bytes,
        });
    }

    ensure_disk_backend_available()?;
    let image = workspace_disk_image_path(workspace);
    let image_metadata = fs::metadata(&image)
        .with_context(|| format!("failed to inspect workspace disk image {}", image.display()))?;
    if !image_metadata.is_file() {
        bail!(
            "workspace disk image {} is not a regular file",
            image.display()
        );
    }
    if image_metadata.len() < current_disk_bytes {
        bail!(
            "workspace disk image {} is smaller than its recorded allocation",
            image.display()
        );
    }
    if image_metadata.len() > new_disk_bytes {
        bail!(
            "workspace disk image {} is already larger than the requested allocation; refusing to shrink it",
            image.display()
        );
    }
    if is_mountpoint(Path::new(&workspace.filesystem_path))? {
        bail!(
            "workspace disk image {} is still mounted; stop the workspace and retry",
            image.display()
        );
    }

    let truncate = Command::new("truncate")
        .args(["-s", &new_disk_bytes.to_string()])
        .arg(&image)
        .output()
        .with_context(|| format!("failed to grow workspace disk image {}", image.display()))?;
    if !truncate.status.success() {
        let stderr = String::from_utf8_lossy(&truncate.stderr);
        bail!(
            "failed to grow workspace disk image {} ({}): {}",
            image.display(),
            truncate.status,
            stderr.trim()
        );
    }

    let resize = Command::new("resize2fs")
        .arg(&image)
        .output()
        .with_context(|| format!("failed to grow ext4 filesystem {}", image.display()))?;
    if !resize.status.success() {
        let stderr = String::from_utf8_lossy(&resize.stderr);
        bail!(
            "grew workspace disk image {} but failed to grow its ext4 filesystem ({}): {}",
            image.display(),
            resize.status,
            stderr.trim()
        );
    }

    let final_size = fs::metadata(&image)
        .with_context(|| format!("failed to verify workspace disk image {}", image.display()))?
        .len();
    if final_size < new_disk_bytes {
        bail!(
            "workspace disk image {} is smaller than the requested allocation after resize",
            image.display()
        );
    }

    Ok(WorkspaceDiskResize {
        previous_bytes: current_disk_bytes,
        new_bytes: new_disk_bytes,
    })
}

pub fn with_workspace_storage_mounted<T, F>(
    workspace: &WorkspaceMetadata,
    operation: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    if !workspace_uses_disk_image(workspace) {
        return operation();
    }

    let mountpoint = Path::new(&workspace.filesystem_path);
    let was_mounted = is_mountpoint(mountpoint)?;
    if !was_mounted {
        ensure_workspace_storage_ready(workspace)?;
    }

    let result = operation();

    if !was_mounted {
        let _ = ensure_workspace_storage_unmounted(workspace);
    }

    result
}

fn mount_disk_image_if_needed(workspace: &WorkspaceMetadata) -> Result<()> {
    let mountpoint = Path::new(&workspace.filesystem_path);
    if is_mountpoint(mountpoint)? {
        return Ok(());
    }
    fs::create_dir_all(mountpoint)
        .with_context(|| format!("failed to create {}", mountpoint.display()))?;
    let image = workspace_disk_image_path(workspace);
    let output = Command::new("mount")
        .args(["-o", "loop"])
        .arg(&image)
        .arg(mountpoint)
        .output()
        .with_context(|| {
            format!(
                "failed to mount quota-backed workspace image {} on {}",
                image.display(),
                mountpoint.display()
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to mount quota-backed workspace image {} on {} ({}): {}",
            image.display(),
            mountpoint.display(),
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

fn initialize_disk_image(workspace: &WorkspaceMetadata) -> Result<()> {
    let image = workspace_disk_image_path(workspace);
    if image.exists() {
        return Ok(());
    }
    let disk_bytes = workspace.limits.disk_bytes.ok_or_else(|| {
        anyhow::anyhow!("workspace '{}' has no disk quota configured", workspace.id)
    })?;
    if let Some(parent) = image.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let truncate = Command::new("truncate")
        .args(["-s", &disk_bytes.to_string()])
        .arg(&image)
        .output()
        .with_context(|| format!("failed to create sparse disk image {}", image.display()))?;
    if !truncate.status.success() {
        let stderr = String::from_utf8_lossy(&truncate.stderr);
        bail!(
            "failed to create sparse disk image {} ({}): {}",
            image.display(),
            truncate.status,
            stderr.trim()
        );
    }
    let mkfs = Command::new("mkfs.ext4")
        .args(["-F", "-q"])
        .arg(&image)
        .output()
        .with_context(|| format!("failed to format ext4 disk image {}", image.display()))?;
    if !mkfs.status.success() {
        let stderr = String::from_utf8_lossy(&mkfs.stderr);
        bail!(
            "failed to format ext4 disk image {} ({}): {}",
            image.display(),
            mkfs.status,
            stderr.trim()
        );
    }
    Ok(())
}

fn ensure_disk_backend_available() -> Result<()> {
    for command in [
        "truncate",
        "mkfs.ext4",
        "resize2fs",
        "mount",
        "umount",
        "mountpoint",
    ] {
        let status = Command::new("sh")
            .args(["-c", &format!("command -v {command} >/dev/null 2>&1")])
            .status()
            .with_context(|| format!("failed to probe availability of {}", command))?;
        if !status.success() {
            bail!(
                "workspace disk quota requires '{}' to be available on the host",
                command
            );
        }
    }
    Ok(())
}

fn is_mountpoint(path: &Path) -> Result<bool> {
    let status = Command::new("mountpoint")
        .arg("-q")
        .arg(path)
        .status()
        .with_context(|| format!("failed to check mountpoint {}", path.display()))?;
    Ok(status.success())
}

pub(crate) fn workspace_disk_image_path(workspace: &WorkspaceMetadata) -> PathBuf {
    PathBuf::from(&workspace.workspace_path).join(DISK_IMAGE_NAME)
}

pub(crate) fn workspace_uses_disk_image(workspace: &WorkspaceMetadata) -> bool {
    workspace.limits.disk_bytes.is_some() && workspace.home_mount_source_path.is_none()
}

#[cfg(test)]
#[path = "../../tests/src/workspace/storage.rs"]
mod tests;
