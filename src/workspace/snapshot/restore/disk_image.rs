//! Restoring a quota-backed workspace filesystem.
//!
//! The workspace filesystem lives inside an ext4 image on a loop device, so it cannot
//! be replaced while it is mounted. The image moves aside, the snapshot copies in, and
//! the image is mounted again to write them: the order is what makes this tier
//! different from the directory one rather than a variation of it.

use std::fs;

use anyhow::{Context, Result};

use super::SnapshotRestorePaths;
use crate::workspace::snapshot::paths::{copy_dir_recursive, reset_path};
use crate::workspace::types::WorkspaceMetadata;

pub(crate) fn restore_disk_image_snapshot(
    workspace: &WorkspaceMetadata,
    paths: SnapshotRestorePaths<'_>,
) -> Result<()> {
    crate::workspace::ensure_workspace_storage_unmounted(workspace)?;

    let image_path = crate::workspace::storage::workspace_disk_image_path(workspace);
    let backup_image = paths.backup_root.join("fs.img");
    let backup_upper = paths.backup_root.join("home-upper");

    if image_path.exists() {
        fs::rename(&image_path, &backup_image).with_context(|| {
            format!(
                "failed to move {} to backup {}",
                image_path.display(),
                backup_image.display()
            )
        })?;
    }
    if paths.upper_path.exists() {
        fs::rename(paths.upper_path, &backup_upper).with_context(|| {
            format!(
                "failed to move {} to backup {}",
                paths.upper_path.display(),
                backup_upper.display()
            )
        })?;
    }
    if paths.work_path.exists() {
        fs::remove_dir_all(paths.work_path)
            .with_context(|| format!("failed to remove {}", paths.work_path.display()))?;
    }
    if paths.merged_path.exists() {
        fs::remove_dir_all(paths.merged_path)
            .with_context(|| format!("failed to remove {}", paths.merged_path.display()))?;
    }
    if paths.fs_path.exists() {
        fs::create_dir_all(paths.fs_path)
            .with_context(|| format!("failed to ensure {}", paths.fs_path.display()))?;
    }

    let restore_result = (|| {
        reset_path(paths.upper_path)?;
        reset_path(paths.work_path)?;
        reset_path(paths.merged_path)?;
        crate::workspace::ensure_workspace_storage_ready(workspace)?;
        let copy_result = copy_dir_recursive(paths.snapshot_fs, paths.fs_path)
            .and_then(|_| copy_dir_recursive(paths.snapshot_home_upper, paths.upper_path));
        let unmount_result = crate::workspace::ensure_workspace_storage_unmounted(workspace);

        copy_result?;
        unmount_result?;
        Ok::<(), anyhow::Error>(())
    })();
    if let Err(err) = restore_result {
        let _ = crate::workspace::ensure_workspace_storage_unmounted(workspace);
        if image_path.exists() {
            if let Err(remove_err) = fs::remove_file(&image_path) {
                tracing::warn!("failed to remove {}: {remove_err:#}", image_path.display());
            }
        }
        if paths.upper_path.exists() {
            if let Err(remove_err) = fs::remove_dir_all(paths.upper_path) {
                tracing::warn!(
                    "failed to remove {}: {remove_err:#}",
                    paths.upper_path.display()
                );
            }
        }
        if backup_image.exists() {
            fs::rename(&backup_image, &image_path).with_context(|| {
                format!(
                    "failed to rollback backup {} -> {}",
                    backup_image.display(),
                    image_path.display()
                )
            })?;
        }
        if backup_upper.exists() {
            fs::rename(&backup_upper, paths.upper_path).with_context(|| {
                format!(
                    "failed to rollback backup {} -> {}",
                    backup_upper.display(),
                    paths.upper_path.display()
                )
            })?;
        }
        return Err(err);
    }

    Ok(())
}
