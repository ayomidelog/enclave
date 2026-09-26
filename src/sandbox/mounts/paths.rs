//! Checking that a sandbox mount target is safe to use before touching it.

use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use super::super::types::SandboxMetadata;

/// Resolve the mount target of an unmount, refusing anything that is not where
/// the sandbox layout says it should be.
///
/// `None` means the sandbox is already gone, which is not an error: there is no
/// mount left to detach. The checks run before the unmount so a symlinked or
/// relocated path cannot be used to detach an unrelated mount.
pub(super) fn validate_unmount_path(metadata: &SandboxMetadata) -> Result<Option<String>> {
    let sandbox_dir = PathBuf::from(&metadata.sandbox_path);
    let sandbox_metadata = match fs::symlink_metadata(&sandbox_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to stat sandbox path {}", sandbox_dir.display()))
        }
    };
    if sandbox_metadata.file_type().is_symlink() {
        bail!(
            "sandbox path {} must not be a symlink",
            sandbox_dir.display()
        );
    }

    let mounted_rootfs_path = PathBuf::from(&metadata.mounted_rootfs_path);
    match fs::symlink_metadata(&mounted_rootfs_path) {
        Ok(path_metadata) if path_metadata.file_type().is_symlink() => {
            bail!(
                "mounted rootfs path {} must not be a symlink",
                mounted_rootfs_path.display()
            )
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to stat mounted rootfs path {}",
                    mounted_rootfs_path.display()
                )
            })
        }
    }
    let canonical_mounted = crate::fsutil::ensure_path_within(
        &sandbox_dir,
        &mounted_rootfs_path,
        "mounted rootfs path",
    )?;

    let rootfs_path = PathBuf::from(&metadata.rootfs_path);
    reject_symlink_if_present("rootfs path", &rootfs_path)?;
    if rootfs_path.exists() {
        let canonical_rootfs =
            crate::fsutil::ensure_path_within(&sandbox_dir, &rootfs_path, "rootfs path")?;
        if canonical_rootfs == canonical_mounted {
            bail!("rootfs and mounted rootfs paths must not be the same");
        }
    }

    Ok(Some(canonical_mounted.to_string_lossy().to_string()))
}

pub(super) fn reject_symlink_if_present(label: &str, path: &std::path::Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("{} {} must not be a symlink", label, path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to stat {} {}", label, path.display()))
        }
    }
}

/// The canonical rootfs and mount targets, refusing a symlink or a path that
/// escapes the sandbox.
pub(super) fn validate_mount_paths(metadata: &SandboxMetadata) -> Result<(String, String)> {
    let sandbox_dir = PathBuf::from(&metadata.sandbox_path);
    let rootfs_path = PathBuf::from(&metadata.rootfs_path);
    let mounted_rootfs_path = PathBuf::from(&metadata.mounted_rootfs_path);

    for (label, path) in [
        ("sandbox path", &sandbox_dir),
        ("rootfs path", &rootfs_path),
        ("mounted rootfs path", &mounted_rootfs_path),
    ] {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("failed to stat {} {}", label, path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("{} {} must not be a symlink", label, path.display());
        }
    }

    let canonical_rootfs =
        crate::fsutil::canonicalize_within(&sandbox_dir, &rootfs_path, "rootfs path")?;
    let canonical_mounted = crate::fsutil::canonicalize_within(
        &sandbox_dir,
        &mounted_rootfs_path,
        "mounted rootfs path",
    )?;
    if canonical_rootfs == canonical_mounted {
        bail!("rootfs and mounted rootfs paths must not be the same");
    }

    Ok((
        canonical_rootfs.to_string_lossy().to_string(),
        canonical_mounted.to_string_lossy().to_string(),
    ))
}
