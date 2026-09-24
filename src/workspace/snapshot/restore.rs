use super::*;

pub fn restore_workspace_snapshot(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    snapshot_name: &str,
) -> Result<WorkspaceMetadata> {
    with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        validate_snapshot_name(snapshot_name)?;

        if workspace.status.may_have_runtime() {
            if let Some(pid) = workspace.runtime_pid {
                session::stop_session(pid, workspace.runtime_starttime_ticks)?;
            }
            set_workspace_stopped(sandbox, &workspace_id)?;
        }

        restore_snapshot_filesystem(&workspace, snapshot_name)?;

        let result = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        Ok(result)
    })
}

pub(crate) fn restore_snapshot_filesystem(
    workspace: &WorkspaceMetadata,
    snapshot_name: &str,
) -> Result<()> {
    let workspace_path = PathBuf::from(&workspace.workspace_path);
    let workspace_path =
        crate::fsutil::ensure_path_within(&workspace_path, &workspace_path, "workspace path")?;
    let snapshot_dir = crate::fsutil::ensure_path_within(
        &workspace_path,
        &snapshot_path(workspace, snapshot_name),
        "snapshot path",
    )?;
    if !snapshot_dir.exists() {
        bail!("snapshot '{}' not found", snapshot_name);
    }
    let snapshot_fs = snapshot_dir.join("fs");
    let snapshot_home_upper = snapshot_dir.join("home-upper");
    if !snapshot_fs.exists() || !snapshot_home_upper.exists() {
        bail!("snapshot '{}' is incomplete", snapshot_name);
    }

    let backup_root = workspace_path.join(".restore-backup");
    if backup_root.exists() {
        fs::remove_dir_all(&backup_root)
            .with_context(|| format!("failed to clean {}", backup_root.display()))?;
    }
    fs::create_dir_all(&backup_root)
        .with_context(|| format!("failed to create {}", backup_root.display()))?;

    let fs_path = crate::fsutil::ensure_path_within(
        &workspace_path,
        Path::new(&workspace.filesystem_path),
        "workspace filesystem path",
    )?;
    let upper_path = crate::fsutil::ensure_path_within(
        &workspace_path,
        Path::new(&workspace.overlay_home_upper_path),
        "workspace home upper path",
    )?;
    let work_path = crate::fsutil::ensure_path_within(
        &workspace_path,
        Path::new(&workspace.overlay_home_work_path),
        "workspace home work path",
    )?;
    let merged_path = crate::fsutil::ensure_path_within(
        &workspace_path,
        Path::new(&workspace.overlay_home_merged_path),
        "workspace home merged path",
    )?;

    let result = if crate::workspace::storage::workspace_uses_disk_image(workspace) {
        restore_disk_image_snapshot(
            workspace,
            SnapshotRestorePaths {
                snapshot_fs: &snapshot_fs,
                snapshot_home_upper: &snapshot_home_upper,
                backup_root: &backup_root,
                fs_path: &fs_path,
                upper_path: &upper_path,
                work_path: &work_path,
                merged_path: &merged_path,
            },
        )
    } else {
        restore_directory_snapshot(SnapshotRestorePaths {
            snapshot_fs: &snapshot_fs,
            snapshot_home_upper: &snapshot_home_upper,
            backup_root: &backup_root,
            fs_path: &fs_path,
            upper_path: &upper_path,
            work_path: &work_path,
            merged_path: &merged_path,
        })
    };

    if backup_root.exists() {
        fs::remove_dir_all(&backup_root)
            .with_context(|| format!("failed to clean {}", backup_root.display()))?;
    }

    result
}

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
