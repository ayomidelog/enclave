use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::types::WorkspaceMetadata;

const DISK_IMAGE_NAME: &str = "fs.img";
const MIN_DISK_BYTES: u64 = 32 * 1024 * 1024;

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
    if !workspace_uses_disk_image(workspace) {
        return Ok(());
    }
    let mountpoint = Path::new(&workspace.filesystem_path);
    if !is_mountpoint(mountpoint)? {
        return Ok(());
    }
    let output = Command::new("umount")
        .arg(mountpoint)
        .output()
        .with_context(|| format!("failed to run umount {}", mountpoint.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to unmount quota-backed workspace storage {} ({}): {}",
            mountpoint.display(),
            output.status,
            stderr.trim()
        );
    }
    Ok(())
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
    for command in ["truncate", "mkfs.ext4", "mount", "umount", "mountpoint"] {
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

fn workspace_disk_image_path(workspace: &WorkspaceMetadata) -> PathBuf {
    PathBuf::from(&workspace.workspace_path).join(DISK_IMAGE_NAME)
}

fn workspace_uses_disk_image(workspace: &WorkspaceMetadata) -> bool {
    workspace.limits.disk_bytes.is_some() && workspace.home_mount_source_path.is_none()
}

#[cfg(test)]
#[path = "../../tests/src/workspace/storage.rs"]
mod tests;
