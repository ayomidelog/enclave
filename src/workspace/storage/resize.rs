//! Growing a workspace's disk image and its filesystem together.
//!
//! The image is the container and the filesystem is what lives inside it, and a
//! resize has to grow both and prove that it did. A failure after the image has
//! grown but before the filesystem has is the one state that must never be left
//! behind, because the workspace then fails its readiness checks on every retry,
//! so every failure past that point restores the previous size.

use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::hostcmd::HostCommand;

use super::definition::MIN_DISK_BYTES;
use super::image::{ensure_disk_backend_available, workspace_disk_image_path};
use super::{ext4, unmount, WorkspaceDiskResize, WorkspaceMetadata};

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
    ensure_disk_image_not_in_use(workspace, &image)?;

    // Grow the container first so the filesystem is checked at the size it is
    // about to be resized to. Every failure after this point restores the
    // previous size, because an image larger than its filesystem leaves the
    // workspace failing its readiness checks on every retry.
    let tool_timeout = Duration::from_secs(120);
    HostCommand::new("truncate")
        .args(["-s", &new_disk_bytes.to_string()])
        .arg(&image)
        .timeout(tool_timeout)
        .run_checked()
        .with_context(|| format!("failed to grow workspace disk image {}", image.display()))?;

    // `resize2fs` refuses to run on a filesystem that was not cleanly
    // unmounted, and a preen-only pass can report success without clearing that
    // state, so force a full check.
    let check = HostCommand::new("e2fsck")
        .args(["-f", "-p"])
        .arg(&image)
        .timeout(tool_timeout)
        .run()
        .with_context(|| {
            format!(
                "failed to check workspace ext4 filesystem {}",
                image.display()
            )
        })?;
    if !matches!(check.status.code(), Some(0 | 1)) {
        let stderr = check.stderr_text();
        return Err(rollback_disk_growth(
            &image,
            current_disk_bytes,
            anyhow::anyhow!(
                "failed to check workspace ext4 filesystem {} ({}): {stderr}",
                image.display(),
                check.status,
            ),
        ));
    }

    let resize = HostCommand::new("resize2fs")
        .arg(&image)
        .timeout(tool_timeout)
        .run()
        .with_context(|| format!("failed to grow ext4 filesystem {}", image.display()))?;
    if !resize.success() {
        let stderr = resize.stderr_text();
        return Err(rollback_disk_growth(
            &image,
            current_disk_bytes,
            anyhow::anyhow!(
                "grew workspace disk image {} but failed to grow its ext4 filesystem ({}): {stderr}",
                image.display(),
                resize.status,
            ),
        ));
    }

    // Prove the filesystem itself grew, not just the image file that holds it.
    let filesystem_bytes = ext4::filesystem_size(&image)?;
    if filesystem_bytes < new_disk_bytes {
        return Err(rollback_disk_growth(
            &image,
            current_disk_bytes,
            anyhow::anyhow!(
                "workspace ext4 filesystem {} reports {} bytes after resize2fs, below the requested {} bytes",
                image.display(),
                filesystem_bytes,
                new_disk_bytes
            ),
        ));
    }

    Ok(WorkspaceDiskResize {
        previous_bytes: current_disk_bytes,
        new_bytes: new_disk_bytes,
    })
}

/// Refuse to resize an image that is still in use anywhere on the host.
///
/// `resize2fs` refuses a filesystem that is still mounted, and Enclave can only
/// see its own mount namespace through the mountpoint check. An image left
/// mounted by another namespace (a runtime that has not fully exited, for
/// example) would otherwise be grown underneath its users and reported as a
/// confusing tool failure, so the loop device and its holders are checked too.
fn ensure_disk_image_not_in_use(workspace: &WorkspaceMetadata, image: &Path) -> Result<()> {
    if crate::fsutil::is_mountpoint(Path::new(&workspace.filesystem_path))? {
        bail!(
            "workspace disk image {} is still mounted; stop the workspace and retry",
            image.display()
        );
    }
    let devices = unmount::loop_devices_for_image(image)?;
    if devices.is_empty() {
        return Ok(());
    }
    let holders = unmount::namespaces_mounting_devices(&devices);
    bail!(
        "workspace disk image {} is still attached to loop device(s) {} and mounted in {}; stop the workspace and retry",
        image.display(),
        devices.join(", "),
        if holders.is_empty() {
            "another mount namespace".to_string()
        } else {
            holders.join(", ")
        }
    )
}

/// Restore an image to its previous allocation after a failed resize.
///
/// The filesystem inside the image is unchanged by a failed resize, so shrinking
/// the image back keeps the recorded allocation, the image size, and the
/// filesystem size consistent and lets the user retry.
fn rollback_disk_growth(image: &Path, previous_bytes: u64, cause: anyhow::Error) -> anyhow::Error {
    let rollback = HostCommand::new("truncate")
        .args(["-s", &previous_bytes.to_string()])
        .arg(image)
        .run();
    match rollback {
        Ok(output) if output.success() => cause.context(format!(
            "restored workspace disk image {} to its previous {} bytes",
            image.display(),
            previous_bytes
        )),
        Ok(output) => cause.context(format!(
            "workspace disk image {} is still larger than its filesystem because restoring it to {} bytes failed ({}): {}",
            image.display(),
            previous_bytes,
            output.status,
            output.stderr_text()
        )),
        Err(error) => cause.context(format!(
            "workspace disk image {} is still larger than its filesystem because restoring it to {} bytes could not run: {error}",
            image.display(),
            previous_bytes
        )),
    }
}
