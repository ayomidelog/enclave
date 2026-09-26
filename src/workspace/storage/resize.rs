//! Growing or shrinking a workspace's disk image and its filesystem together.
//!
//! The image is the container and the filesystem is what lives inside it, and a
//! resize has to change both and prove that it did. The state that must never be
//! left behind is an image whose size disagrees with its filesystem: the workspace
//! then fails every retry with an error that names neither. So a resize either
//! completes or restores what was there before, and the result is verified from the
//! ext4 superblock rather than from the image file size.
//!
//! Growing and shrinking are the same two steps in opposite order. A grow makes the
//! container bigger first, because the filesystem cannot be resized past the device
//! it lives on. A shrink makes the filesystem smaller first, because `truncate`
//! would otherwise cut the filesystem off mid-block.

use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::hostcmd::HostCommand;

use super::definition::MIN_DISK_BYTES;
use super::ext4;
use super::image::{ensure_disk_backend_available, workspace_disk_image_path};
use super::{unmount, WorkspaceDiskResize, WorkspaceMetadata};

/// How long the filesystem tools get.
///
/// A quota-backed image is large, so these get longer than the default host-command
/// deadline while still being bounded.
const TOOL_TIMEOUT: Duration = Duration::from_secs(120);

/// A disk resize that has been checked and is ready to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedDiskResize {
    pub(crate) previous_bytes: u64,
    pub(crate) new_bytes: u64,
    /// The size the image file is now, which is what a failure restores it to.
    pub(crate) image_bytes: u64,
}

impl PlannedDiskResize {
    /// Whether this resize changes anything.
    pub fn is_change(&self) -> bool {
        self.previous_bytes != self.new_bytes
    }
}

/// Check a requested disk allocation without touching anything.
///
/// Split from the resize itself because the caller has to know a request is
/// impossible *before* it stops the workspace to perform it. A resize that stopped a
/// running runtime and then refused the size would leave the workspace down for a
/// request it could have rejected outright.
///
/// What is checked here is the request: whether this workspace can have a managed
/// disk at all, whether the size is one Enclave accepts, and whether the image can
/// hold it. Whether anything currently holds the image is deliberately *not* checked,
/// because a running workspace holds its own image and this runs before the stop that
/// releases it. [`resize_workspace_disk_allocation`] checks that, after the caller has
/// stopped the runtime.
pub fn plan_workspace_disk_resize(
    workspace: &WorkspaceMetadata,
    new_disk_bytes: u64,
) -> Result<PlannedDiskResize> {
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
    if new_disk_bytes == current_disk_bytes {
        return Ok(PlannedDiskResize {
            previous_bytes: current_disk_bytes,
            new_bytes: current_disk_bytes,
            image_bytes: 0,
        });
    }

    ensure_disk_backend_available()?;
    let image = workspace_disk_image_path(workspace);
    let image_bytes = validate_image(workspace, &image, current_disk_bytes, new_disk_bytes)?;
    Ok(PlannedDiskResize {
        previous_bytes: current_disk_bytes,
        new_bytes: new_disk_bytes,
        image_bytes,
    })
}

/// Resize a workspace's disk allocation, in either direction.
///
/// The workspace must be stopped: a mounted filesystem cannot be resized, and the
/// caller stops the runtime before calling this. Everything the request could be
/// refused for is checked by [`plan_workspace_disk_resize`] first, so a plan that was
/// accepted is not rejected half way through.
pub fn resize_workspace_disk_allocation(
    workspace: &WorkspaceMetadata,
    new_disk_bytes: u64,
) -> Result<WorkspaceDiskResize> {
    let plan = plan_workspace_disk_resize(workspace, new_disk_bytes)?;
    if !plan.is_change() {
        return Ok(WorkspaceDiskResize {
            previous_bytes: plan.previous_bytes,
            new_bytes: plan.new_bytes,
        });
    }

    let image = workspace_disk_image_path(workspace);
    // The runtime has been stopped by the caller, so the image should be free. A
    // holder here is another namespace, which is a host state this request cannot fix
    // and which is named rather than reported as a tool failure.
    ensure_disk_image_not_in_use(workspace, &image)?;
    if plan.new_bytes > plan.previous_bytes {
        grow_filesystem(&image, plan.image_bytes, plan.new_bytes)?;
    } else {
        shrink_filesystem(&image, plan.image_bytes, plan.new_bytes)?;
    }

    Ok(WorkspaceDiskResize {
        previous_bytes: plan.previous_bytes,
        new_bytes: plan.new_bytes,
    })
}

/// Check the image file before anything is written to it.
///
/// Returns the size the file is now, which is what a failed resize is restored to.
fn validate_image(
    workspace: &WorkspaceMetadata,
    image: &Path,
    current_disk_bytes: u64,
    new_disk_bytes: u64,
) -> Result<u64> {
    let metadata = fs::metadata(image)
        .with_context(|| format!("failed to inspect workspace disk image {}", image.display()))?;
    if !metadata.is_file() {
        bail!(
            "workspace disk image {} is not a regular file",
            image.display()
        );
    }
    let image_bytes = metadata.len();
    if image_bytes < current_disk_bytes {
        bail!(
            "workspace disk image {} is smaller than its recorded allocation",
            image.display()
        );
    }
    if new_disk_bytes > current_disk_bytes && image_bytes > new_disk_bytes {
        bail!(
            "workspace disk image {} is already larger than the requested allocation; refusing to shrink it through a grow",
            image.display()
        );
    }
    if new_disk_bytes < current_disk_bytes {
        // A shrink has to be able to describe the image it is about to cut down, so
        // the filesystem is read before anything is written. A file that is not an
        // ext filesystem, or whose superblock cannot be read, fails here rather than
        // half way through.
        let geometry = ext4::geometry(image)?;
        if geometry.size_bytes() > image_bytes {
            bail!(
                "workspace '{}' disk image {} holds a {} byte filesystem in a {} byte file; the image is already inconsistent",
                workspace.id,
                image.display(),
                geometry.size_bytes(),
                image_bytes
            );
        }
    }
    Ok(image_bytes)
}

/// Grow the image file, then the filesystem inside it.
fn grow_filesystem(image: &Path, previous_image_bytes: u64, new_disk_bytes: u64) -> Result<()> {
    // Grow the container first so the filesystem is checked at the size it is
    // about to be resized to. Every failure after this point restores the
    // previous size, because an image larger than its filesystem leaves the
    // workspace failing its readiness checks on every retry.
    HostCommand::new("truncate")
        .args(["-s", &new_disk_bytes.to_string()])
        .arg(image)
        .timeout(TOOL_TIMEOUT)
        .run_checked()
        .with_context(|| format!("failed to grow workspace disk image {}", image.display()))?;

    let restore = || restore_image_size(image, previous_image_bytes);

    check_filesystem(image).map_err(|error| restore_then(image, restore, error))?;

    let resize = HostCommand::new("resize2fs")
        .arg(image)
        .timeout(TOOL_TIMEOUT)
        .run()
        .with_context(|| format!("failed to grow ext4 filesystem {}", image.display()))?;
    if !resize.success() {
        let stderr = resize.stderr_text();
        return Err(restore_then(
            image,
            restore,
            anyhow::anyhow!(
                "grew workspace disk image {} but failed to grow its ext4 filesystem ({}): {stderr}",
                image.display(),
                resize.status,
            ),
        ));
    }

    // Prove the filesystem itself grew, not just the image file that holds it.
    let filesystem_bytes = ext4::filesystem_size(image)?;
    if filesystem_bytes < new_disk_bytes {
        return Err(restore_then(
            image,
            restore,
            anyhow::anyhow!(
                "workspace ext4 filesystem {} reports {} bytes after resize2fs, below the requested {} bytes",
                image.display(),
                filesystem_bytes,
                new_disk_bytes
            ),
        ));
    }
    Ok(())
}

/// Shrink the filesystem inside the image, then the image file.
fn shrink_filesystem(image: &Path, previous_image_bytes: u64, new_disk_bytes: u64) -> Result<()> {
    // The filesystem has to be checked before it can be shrunk, and the check has to
    // be a forced full pass: `resize2fs` refuses a filesystem that was not cleanly
    // unmounted, and a preen-only pass can report success without clearing that
    // state. Nothing has been written yet, so a failure here needs no restore.
    check_filesystem(image)?;

    let geometry = ext4::geometry(image)?;
    // `resize2fs` takes a block count, so the request is rounded down to a whole
    // number of blocks. That is what keeps the filesystem and the image file the
    // same size instead of leaving a partial block at the end.
    let target_blocks = geometry.blocks_in(new_disk_bytes);
    let target_bytes = target_blocks.saturating_mul(geometry.block_size);
    if target_bytes < new_disk_bytes {
        // Only reachable for a request that is not a whole number of blocks, which
        // the CLI cannot express, but rounding a shrink below the floor would be a
        // silent loss of space.
        bail!(
            "workspace disk allocation must be a multiple of the {} byte block size of {}",
            geometry.block_size,
            image.display()
        );
    }

    let minimum_bytes = minimum_filesystem_bytes(image, geometry.block_size)?;
    if target_bytes < minimum_bytes {
        bail!(
            "cannot shrink workspace disk image {} to {} MiB: the filesystem holds too much data for less than {} MiB",
            image.display(),
            new_disk_bytes / (1024 * 1024),
            minimum_bytes.div_ceil(1024 * 1024),
        );
    }

    let resize = HostCommand::new("resize2fs")
        .arg(image)
        .arg(target_blocks.to_string())
        .timeout(TOOL_TIMEOUT)
        .run()
        .with_context(|| format!("failed to shrink ext4 filesystem {}", image.display()))?;
    if !resize.success() {
        let stderr = resize.stderr_text();
        return Err(restore_then(
            image,
            || restore_image_size(image, previous_image_bytes),
            anyhow::anyhow!(
                "failed to shrink the ext4 filesystem in {} to {} blocks ({}): {stderr}",
                image.display(),
                target_blocks,
                resize.status,
            ),
        ));
    }

    // The filesystem is now smaller than the file that holds it. Cutting the file
    // down is the last step, and a failure here is recoverable: the filesystem still
    // fits, so growing it back to the file it is in restores the previous state.
    if let Err(error) = HostCommand::new("truncate")
        .args(["-s", &target_bytes.to_string()])
        .arg(image)
        .timeout(TOOL_TIMEOUT)
        .run_checked()
    {
        return Err(restore_then(
            image,
            || restore_image_size(image, previous_image_bytes),
            error.context(format!(
                "failed to shrink workspace disk image {}",
                image.display()
            )),
        ));
    }

    // Prove both halves reached the size that was asked for.
    let resized = ext4::geometry(image)?;
    let image_bytes = fs::metadata(image)
        .with_context(|| format!("failed to inspect workspace disk image {}", image.display()))?
        .len();
    if resized.block_count != target_blocks || image_bytes != target_bytes {
        return Err(restore_then(
            image,
            || restore_image_size(image, previous_image_bytes),
            anyhow::anyhow!(
                "workspace disk image {} is {} bytes holding {} blocks after a shrink to {} blocks; the image and its filesystem disagree",
                image.display(),
                image_bytes,
                resized.block_count,
                target_blocks,
            ),
        ));
    }
    Ok(())
}

/// Run a full filesystem check, accepting the two exit codes that mean it is usable.
///
/// `e2fsck` exits 0 for a clean filesystem and 1 for one it repaired. Anything else
/// is a filesystem `resize2fs` would refuse, so it is reported here where the reason
/// is still visible.
fn check_filesystem(image: &Path) -> Result<()> {
    let check = HostCommand::new("e2fsck")
        .args(["-f", "-p"])
        .arg(image)
        .timeout(TOOL_TIMEOUT)
        .run()
        .with_context(|| {
            format!(
                "failed to check workspace ext4 filesystem {}",
                image.display()
            )
        })?;
    if matches!(check.status.code(), Some(0 | 1)) {
        return Ok(());
    }
    let stderr = check.stderr_text();
    bail!(
        "failed to check workspace ext4 filesystem {} ({}): {stderr}",
        image.display(),
        check.status,
    )
}

/// The smallest the filesystem in `image` can be made, from `resize2fs -P`.
///
/// This is what turns "the filesystem holds more data than that" into a message
/// naming the floor, rather than a `resize2fs` refusal the operator has to decode.
fn minimum_filesystem_bytes(image: &Path, block_size: u64) -> Result<u64> {
    let output = HostCommand::new("resize2fs")
        .args(["-P"])
        .arg(image)
        .timeout(TOOL_TIMEOUT)
        .run()
        .with_context(|| {
            format!(
                "failed to measure the smallest size of the filesystem in {}",
                image.display()
            )
        })?;
    let stdout = output.stdout_text();
    let blocks = stdout
        .lines()
        .find(|line| line.contains("minimum size"))
        .and_then(|line| line.rsplit(':').next())
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok());
    let Some(blocks) = blocks else {
        bail!(
            "could not read the smallest size of the filesystem in {} from resize2fs: {}",
            image.display(),
            stdout.trim()
        );
    };
    Ok(blocks.saturating_mul(block_size))
}

/// Put the image back to a previous size and report `cause` either way.
///
/// The restore is best effort: the cause is what the caller has to act on, so a
/// restore that also fails is reported beside it rather than replacing it.
fn restore_then<F>(image: &Path, restore: F, cause: anyhow::Error) -> anyhow::Error
where
    F: FnOnce() -> Result<()>,
{
    match restore() {
        Ok(()) => cause.context(format!(
            "restored workspace disk image {} to its previous size",
            image.display()
        )),
        Err(error) => cause.context(format!(
            "could not restore workspace disk image {}: {error:#}",
            image.display()
        )),
    }
}

/// Restore an image to a size it had before, growing the filesystem to fill it again
/// if that is needed.
///
/// The file size is the part that has to be restored: the invariant is that the
/// image and the record agree, and a filesystem smaller than its image is a state
/// the next resize can read and work with. Growing the filesystem back is therefore
/// best effort. It is needed after a failed shrink, where the filesystem was made
/// smaller than the file it is in, and it is not needed after a failed grow, where
/// the filesystem never moved. An image that holds no filesystem at all — a failure
/// that happened before the first `mkfs`, or a file that was replaced — has nothing
/// to grow, so a `resize2fs` that fails there is reported and does not undo the
/// restore.
fn restore_image_size(image: &Path, previous_image_bytes: u64) -> Result<()> {
    HostCommand::new("truncate")
        .args(["-s", &previous_image_bytes.to_string()])
        .arg(image)
        .timeout(TOOL_TIMEOUT)
        .run_checked()
        .with_context(|| format!("failed to restore the size of {}", image.display()))?;
    // `resize2fs` without a size grows the filesystem to fill the file it is in.
    match HostCommand::new("resize2fs")
        .arg(image)
        .timeout(TOOL_TIMEOUT)
        .run()
    {
        Ok(output) if output.success() => {}
        Ok(output) => tracing::warn!(
            "restored the size of {} but could not grow its filesystem back: {}",
            image.display(),
            output.stderr_text()
        ),
        Err(error) => tracing::warn!(
            "restored the size of {} but could not run resize2fs on it: {error:#}",
            image.display()
        ),
    }
    Ok(())
}

/// Refuse to resize an image that is still in use anywhere on the host.
///
/// `resize2fs` refuses a filesystem that is still mounted, and Enclave can only
/// see its own mount namespace through the mountpoint check. An image left
/// mounted by another namespace (a runtime that has not fully exited, for
/// example) would otherwise be resized underneath its users and reported as a
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
