//! Restoring a directory-backed workspace filesystem.
//!
//! The workspace filesystem and its home overlay are ordinary directories, so a
//! restore is a swap: the live directories move aside, the snapshot copies land in
//! their place, and a failure puts the originals back.

use std::fs;

use anyhow::{Context, Result};

use super::SnapshotRestorePaths;
use crate::workspace::snapshot::paths::{copy_dir_recursive, reset_path};

pub(crate) fn restore_directory_snapshot(paths: SnapshotRestorePaths<'_>) -> Result<()> {
    let backup_fs = paths.backup_root.join("fs");
    let backup_upper = paths.backup_root.join("home-upper");

    if paths.fs_path.exists() {
        fs::rename(paths.fs_path, &backup_fs).with_context(|| {
            format!(
                "failed to move {} to backup {}",
                paths.fs_path.display(),
                backup_fs.display()
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

    let restore_result = (|| {
        reset_path(paths.fs_path)?;
        reset_path(paths.upper_path)?;
        reset_path(paths.work_path)?;
        reset_path(paths.merged_path)?;
        copy_dir_recursive(paths.snapshot_fs, paths.fs_path)?;
        copy_dir_recursive(paths.snapshot_home_upper, paths.upper_path)?;
        Ok::<(), anyhow::Error>(())
    })();
    if let Err(err) = restore_result {
        if paths.fs_path.exists() {
            if let Err(remove_err) = fs::remove_dir_all(paths.fs_path) {
                tracing::warn!(
                    "failed to remove {}: {remove_err:#}",
                    paths.fs_path.display()
                );
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
        if backup_fs.exists() {
            fs::rename(&backup_fs, paths.fs_path).with_context(|| {
                format!(
                    "failed to rollback backup {} -> {}",
                    backup_fs.display(),
                    paths.fs_path.display()
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
