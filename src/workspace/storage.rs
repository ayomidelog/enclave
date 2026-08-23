use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};

use super::types::WorkspaceMetadata;

const DISK_IMAGE_NAME: &str = "fs.img";
const MIN_DISK_BYTES: u64 = 32 * 1024 * 1024;
static DISK_BACKEND_CHECK: OnceLock<Result<(), String>> = OnceLock::new();

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
        ensure_root_overlay_layout(workspace)?;
    }
    Ok(())
}

pub(crate) fn root_overlay_paths(
    workspace: &WorkspaceMetadata,
) -> Option<(PathBuf, PathBuf, PathBuf)> {
    if !workspace_uses_disk_image(workspace) {
        return None;
    }
    Some((
        PathBuf::from(&workspace.filesystem_path).join("root-upper"),
        PathBuf::from(&workspace.filesystem_path).join("root-work"),
        PathBuf::from(&workspace.workspace_path).join("root-merged"),
    ))
}

fn ensure_root_overlay_layout(workspace: &WorkspaceMetadata) -> Result<()> {
    let Some((upper, work, merged)) = root_overlay_paths(workspace) else {
        return Ok(());
    };
    for path in [&upper, &work, &merged] {
        fs::create_dir_all(path).with_context(|| {
            format!("failed to create root overlay directory {}", path.display())
        })?;
    }
    Ok(())
}

pub fn ensure_workspace_storage_unmounted(workspace: &WorkspaceMetadata) -> Result<()> {
    let workspace_root = Path::new(&workspace.workspace_path);
    let snapshot = crate::fsutil::MountInfoSnapshot::load()?;
    let mountpoints = snapshot.at_or_below(workspace_root);

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
    let mountpoints = crate::fsutil::MountInfoSnapshot::load()?.at_or_below(root);
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

#[cfg(test)]
fn parse_mountinfo_mountpoints(mountinfo: &str) -> Vec<PathBuf> {
    crate::fsutil::MountInfoSnapshot::parse(mountinfo).at_or_below(Path::new("/"))
}

fn unmount_workspace_path(path: &Path, owner_is_dead: bool) -> Result<()> {
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

fn unmount_path(path: &Path, flags: i32) -> std::io::Result<()> {
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
    if crate::fsutil::is_mountpoint(Path::new(&workspace.filesystem_path))? {
        bail!(
            "workspace disk image {} is still mounted; stop the workspace and retry",
            image.display()
        );
    }

    let check = Command::new("e2fsck")
        .args(["-p"])
        .arg(&image)
        .output()
        .with_context(|| {
            format!(
                "failed to check workspace ext4 filesystem {}",
                image.display()
            )
        })?;
    if !matches!(check.status.code(), Some(0 | 1)) {
        let stderr = String::from_utf8_lossy(&check.stderr);
        bail!(
            "failed to check workspace ext4 filesystem {} ({}): {}",
            image.display(),
            check.status,
            stderr.trim()
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
    let was_mounted = crate::fsutil::is_mountpoint(mountpoint)?;
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
    if crate::fsutil::is_mountpoint(mountpoint)? {
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
    let result = DISK_BACKEND_CHECK.get_or_init(|| {
        for command in ["truncate", "mkfs.ext4", "resize2fs", "e2fsck", "mount"] {
            let status = Command::new("sh")
                .args(["-c", &format!("command -v {command} >/dev/null 2>&1")])
                .status()
                .map_err(|error| format!("failed to probe availability of {command}: {error}"))?;
            if !status.success() {
                return Err(format!(
                    "workspace disk quota requires '{}' to be available on the host",
                    command
                ));
            }
        }
        Ok(())
    });
    result
        .as_ref()
        .map_err(|error| anyhow::anyhow!(error))
        .copied()
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
