use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::types::WorkspaceMetadata;

const DISK_IMAGE_NAME: &str = "fs.img";
const MIN_DISK_BYTES: u64 = 32 * 1024 * 1024;
const LOOP_DETACH_TIMEOUT: Duration = Duration::from_secs(2);
const LOOP_DETACH_POLL_INTERVAL: Duration = Duration::from_millis(50);
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

mod image;
mod mount;
mod tmp;
mod unmount;

pub use mount::{create_workspace_storage, ensure_workspace_storage_ready};
pub use unmount::ensure_workspace_storage_unmounted;

// Used by workspace lifecycle modules and their tests.
pub(crate) use image::{workspace_disk_image_path, workspace_uses_disk_image};
pub(crate) use mount::with_workspace_storage_mounted;
pub(crate) use tmp::reset_workspace_tmp;
pub(crate) use unmount::ensure_workspace_storage_unmounted_many;
pub(crate) use unmount::unmount_mounts_at_or_below_excluding;

pub(crate) use image::ensure_disk_backend_available;
pub(crate) use mount::root_overlay_paths;

// Entry points the storage tests reach through `super::`.
#[cfg(test)]
pub(crate) use tmp::{clear_workspace_tmp_contents, reset_mounted_workspace_tmp};
#[cfg(test)]
pub(crate) use unmount::{
    parse_loop_devices, parse_mountinfo_mountpoints, unmount_error, workspace_owner_is_dead,
};

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

#[cfg(test)]
#[path = "../../tests/src/workspace/storage.rs"]
mod tests;
