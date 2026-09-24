use super::*;

pub(crate) fn mount_disk_image_if_needed(workspace: &WorkspaceMetadata) -> Result<()> {
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

pub(crate) fn initialize_disk_image(workspace: &WorkspaceMetadata) -> Result<()> {
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

pub(crate) fn ensure_disk_backend_available() -> Result<()> {
    let result = DISK_BACKEND_CHECK.get_or_init(|| {
        for command in [
            "truncate",
            "mkfs.ext4",
            "resize2fs",
            "e2fsck",
            "mount",
            "losetup",
        ] {
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
