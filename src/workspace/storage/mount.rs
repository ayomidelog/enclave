use super::image::{
    ensure_disk_backend_available, initialize_disk_image, mount_disk_image_if_needed,
};
use super::*;

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
        ensure_root_overlay_layout(workspace)?;
    }
    Ok(())
}

pub(crate) fn root_overlay_paths(
    workspace: &WorkspaceMetadata,
) -> Option<(PathBuf, PathBuf, PathBuf)> {
    if !workspace_uses_disk_image(workspace) {
        return None;
    }
    Some((
        PathBuf::from(&workspace.filesystem_path).join("root-upper"),
        PathBuf::from(&workspace.filesystem_path).join("root-work"),
        PathBuf::from(&workspace.workspace_path).join("root-merged"),
    ))
}

pub(crate) fn ensure_root_overlay_layout(workspace: &WorkspaceMetadata) -> Result<()> {
    let Some((upper, work, merged)) = root_overlay_paths(workspace) else {
        return Ok(());
    };
    for path in [&upper, &work, &merged] {
        fs::create_dir_all(path).with_context(|| {
            format!("failed to create root overlay directory {}", path.display())
        })?;
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
    let was_mounted = crate::fsutil::is_mountpoint(mountpoint)?;
    if !was_mounted {
        ensure_workspace_storage_ready(workspace)?;
    }

    let result = operation();

    if !was_mounted {
        let _ = ensure_workspace_storage_unmounted(workspace);
    }

    result
}
