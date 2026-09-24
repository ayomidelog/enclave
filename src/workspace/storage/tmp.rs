use super::*;

pub(crate) fn reset_workspace_tmp(workspace: &WorkspaceMetadata) -> Result<()> {
    if !workspace_uses_disk_image(workspace) {
        return Ok(());
    }
    // `/tmp` for a quota-backed workspace lives inside the workspace disk
    // image. Clearing the host mountpoint after the image is unmounted would
    // leave the image untouched, so mount the image when the caller is not
    // already holding it.
    with_workspace_storage_mounted(workspace, || reset_mounted_workspace_tmp(workspace))
}

pub(crate) fn reset_mounted_workspace_tmp(workspace: &WorkspaceMetadata) -> Result<()> {
    let workspace_root = PathBuf::from(&workspace.filesystem_path);
    if !crate::fsutil::is_mountpoint(&workspace_root)? {
        bail!(
            "refusing to reset workspace /tmp: {} is not mounted",
            workspace_root.display()
        );
    }
    clear_workspace_tmp_contents(&workspace_root)
}

/// Remove every entry under `<workspace filesystem>/tmp`, keeping the directory
/// itself. The caller owns the storage mount; this only touches paths.
pub(crate) fn clear_workspace_tmp_contents(workspace_root: &Path) -> Result<()> {
    let tmp_path = workspace_root.join("tmp");
    let canonical_root = fs::canonicalize(workspace_root).with_context(|| {
        format!(
            "failed to resolve workspace filesystem {}",
            workspace_root.display()
        )
    })?;
    let canonical_tmp =
        crate::fsutil::ensure_path_within(&canonical_root, &tmp_path, "workspace tmp")?;
    let metadata = match fs::symlink_metadata(&canonical_tmp) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(&canonical_tmp).with_context(|| {
                format!("failed to create workspace tmp {}", canonical_tmp.display())
            })?;
            return Ok(());
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect workspace tmp {}",
                    canonical_tmp.display()
                )
            })
        }
    };
    if metadata.file_type().is_symlink() {
        bail!(
            "workspace tmp path must not be a symlink: {}",
            canonical_tmp.display()
        );
    }

    fs::create_dir_all(&canonical_tmp)
        .with_context(|| format!("failed to create workspace tmp {}", canonical_tmp.display()))?;
    for entry in fs::read_dir(&canonical_tmp)
        .with_context(|| format!("failed to read workspace tmp {}", canonical_tmp.display()))?
    {
        let path = entry
            .with_context(|| {
                format!(
                    "failed to inspect workspace tmp {}",
                    canonical_tmp.display()
                )
            })?
            .path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("failed to inspect workspace tmp entry {}", path.display()))?;
        if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(&path).with_context(|| {
                format!(
                    "failed to remove workspace tmp directory {}",
                    path.display()
                )
            })?;
        } else {
            fs::remove_file(&path).with_context(|| {
                format!("failed to remove workspace tmp entry {}", path.display())
            })?;
        }
    }
    Ok(())
}
