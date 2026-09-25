//! The host directory a workspace's home is served from.
//!
//! A workspace either owns its home, in which case Enclave prepares a base
//! skeleton and an overlay for it, or the operator points it at a host
//! directory, in which case the path has to be validated before anything is
//! mounted.

use super::*;

pub(super) fn resolve_home_mount_source(home_mount_source: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = home_mount_source else {
        return Ok(None);
    };

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("workspace host path (workspace.<id>.path / daemon 'path') must not be empty");
    }

    let host_path = Path::new(trimmed);
    if !host_path.is_absolute() {
        bail!("workspace host path (workspace.<id>.path / daemon 'path') must be absolute");
    }
    if !host_path.is_dir() {
        bail!(
            "workspace host path (workspace.<id>.path / daemon 'path') '{}' must be an existing directory",
            trimmed
        );
    }

    let canonical = host_path.canonicalize().with_context(|| {
        format!(
            "failed to resolve workspace host path (workspace.<id>.path / daemon 'path') '{}'",
            trimmed
        )
    })?;
    Ok(Some(canonical.to_string_lossy().to_string()))
}

pub(crate) fn ensure_traversable_directory_permissions(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to stat directory {}", path.display()))?;
    if !metadata.is_dir() {
        bail!("expected directory at {}", path.display());
    }

    let current_mode = metadata.permissions().mode();
    let mut perms = metadata.permissions();

    perms.set_mode((current_mode & !0o777) | 0o755);
    fs::set_permissions(path, perms).with_context(|| {
        format!(
            "failed to set directory permissions to 0755 for {}",
            path.display()
        )
    })?;
    Ok(())
}

pub(super) fn ensure_home_base_skeleton(home_base_path: &Path) -> Result<()> {
    fs::create_dir_all(home_base_path)
        .with_context(|| format!("failed to create home base {}", home_base_path.display()))?;
    let readme = home_base_path.join("README");
    if !readme.exists() {
        fs::write(
            &readme,
            "Shared home base layer for all workspaces in this sandbox.\n",
        )
        .with_context(|| format!("failed to write {}", readme.display()))?;
    }
    Ok(())
}
