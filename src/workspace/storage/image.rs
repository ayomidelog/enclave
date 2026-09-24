use super::*;

use crate::hostcmd::HostCommand;

pub(crate) fn mount_disk_image_if_needed(workspace: &WorkspaceMetadata) -> Result<()> {
    let mountpoint = Path::new(&workspace.filesystem_path);
    if crate::fsutil::is_mountpoint(mountpoint)? {
        return Ok(());
    }
    fs::create_dir_all(mountpoint)
        .with_context(|| format!("failed to create {}", mountpoint.display()))?;
    let image = workspace_disk_image_path(workspace);
    HostCommand::new("mount")
        .args(["-o", "loop"])
        .arg(&image)
        .arg(mountpoint)
        .run_checked()
        .with_context(|| {
            format!(
                "failed to mount quota-backed workspace image {} on {}",
                image.display(),
                mountpoint.display()
            )
        })?;
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
    // A quota-backed image is large, so the filesystem tools get a longer
    // deadline than the default while still being bounded.
    let tool_timeout = Duration::from_secs(120);
    HostCommand::new("truncate")
        .args(["-s", &disk_bytes.to_string()])
        .arg(&image)
        .timeout(tool_timeout)
        .run_checked()
        .with_context(|| format!("failed to create sparse disk image {}", image.display()))?;
    HostCommand::new("mkfs.ext4")
        .args(["-F", "-q"])
        .arg(&image)
        .timeout(tool_timeout)
        .run_checked()
        .with_context(|| format!("failed to format ext4 disk image {}", image.display()))?;
    Ok(())
}

/// The host programs a quota-backed workspace needs, checked once per process.
const DISK_BACKEND_TOOLS: &[&str] = &[
    "truncate",
    "mkfs.ext4",
    "resize2fs",
    "e2fsck",
    "mount",
    "losetup",
];

pub(crate) fn ensure_disk_backend_available() -> Result<()> {
    let result = DISK_BACKEND_CHECK.get_or_init(|| {
        for tool in DISK_BACKEND_TOOLS {
            if crate::hostcmd::command_on_path(tool).is_none() {
                return Err(format!(
                    "workspace disk quota requires '{tool}' to be available on the host"
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
