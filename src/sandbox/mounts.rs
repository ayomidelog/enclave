use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use super::types::SandboxMetadata;
use super::util::command_failure_detail;

pub fn ensure_rootfs_mounted(metadata: &SandboxMetadata) -> Result<()> {
    let (rootfs_path, mounted_rootfs_path) = validate_mount_paths(metadata)?;
    if is_mountpoint(&mounted_rootfs_path)? {
        return Ok(());
    }

    let output = Command::new("mount")
        .arg("--bind")
        .arg(&rootfs_path)
        .arg(&mounted_rootfs_path)
        .output()
        .context("failed to run mount --bind")?;
    if !output.status.success() {
        return Err(anyhow!(
            "failed to mount sandbox rootfs ({}): {}",
            output.status,
            command_failure_detail(&output)
        ));
    }

    let output = Command::new("mount")
        .arg("--make-private")
        .arg(&mounted_rootfs_path)
        .output()
        .context("failed to run mount --make-private")?;
    if !output.status.success() {
        return Err(anyhow!(
            "failed to mark mount private ({}): {}",
            output.status,
            command_failure_detail(&output)
        ));
    }

    Ok(())
}

pub fn ensure_rootfs_unmounted(metadata: &SandboxMetadata) -> Result<()> {
    let Some(mounted_rootfs_path) = validate_unmount_path(metadata)? else {
        return Ok(());
    };
    if !is_mountpoint(&mounted_rootfs_path)? {
        return Ok(());
    }

    let output = Command::new("umount")
        .arg(&mounted_rootfs_path)
        .output()
        .context("failed to run umount")?;
    if !output.status.success() {
        let detail = command_failure_detail(&output);
        if is_already_unmounted_error(&detail) {
            return Ok(());
        }
        return Err(anyhow!(
            "failed to unmount sandbox rootfs ({}): {}",
            output.status,
            detail
        ));
    }
    if is_mountpoint(&mounted_rootfs_path)? {
        bail!(
            "sandbox rootfs mount {} is still present after umount",
            mounted_rootfs_path
        );
    }
    Ok(())
}

fn validate_unmount_path(metadata: &SandboxMetadata) -> Result<Option<String>> {
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

fn reject_symlink_if_present(label: &str, path: &std::path::Path) -> Result<()> {
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

fn is_already_unmounted_error(detail: &str) -> bool {
    let detail = detail.to_ascii_lowercase();
    detail.contains("enoent")
        || detail.contains("einval")
        || detail.contains("no such file")
        || detail.contains("invalid argument")
        || detail.contains("not mounted")
        || detail.contains("no mount point")
}

fn is_mountpoint(path: &str) -> Result<bool> {
    let status = Command::new("mountpoint")
        .arg("-q")
        .arg(path)
        .status()
        .with_context(|| format!("failed to check mountpoint {}", path))?;
    Ok(status.success())
}

fn validate_mount_paths(metadata: &SandboxMetadata) -> Result<(String, String)> {
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

#[cfg(test)]
#[path = "../../tests/src/sandbox/mounts.rs"]
mod tests;
