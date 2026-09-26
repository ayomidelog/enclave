//! Checking the storage a workspace declared before anything is created for it.

use std::fs;
use std::path::Path;

use anyhow::{bail, Result};

use super::image::workspace_uses_disk_image;
use super::WorkspaceMetadata;

/// The smallest disk quota a workspace may declare.
pub(crate) const MIN_DISK_BYTES: u64 = 32 * 1024 * 1024;

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

/// Verify that the workspace source the session will bind into `/home` is usable.
///
/// The session resolves this path inside its own mount namespace and pivot root,
/// where a failure can only be reported as an opaque path error. Checking it
/// before the launch names the workspace and the host path that has to be
/// repaired instead.
pub fn verify_workspace_source(workspace: &WorkspaceMetadata) -> Result<()> {
    let source = workspace
        .home_mount_source_path
        .as_deref()
        .unwrap_or(&workspace.filesystem_path);
    let path = Path::new(source);
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => bail!(
            "workspace '{}' source path {} is not a directory; recreate the workspace or point it at a directory",
            workspace.id,
            source
        ),
        Err(error) => bail!(
            "workspace '{}' source path {} is unavailable ({error}); run 'enclave workspace stop {} {}' and start it again, or 'enclave doctor --repair'",
            workspace.id,
            source,
            workspace.sandbox_id,
            workspace.id
        ),
    }
    if workspace_uses_disk_image(workspace) && !crate::fsutil::is_mountpoint(path)? {
        bail!(
            "workspace '{}' quota-backed storage {} is not mounted; run 'enclave workspace stop {} {}' and start it again, or 'enclave doctor --repair'",
            workspace.id,
            source,
            workspace.sandbox_id,
            workspace.id
        );
    }
    Ok(())
}
