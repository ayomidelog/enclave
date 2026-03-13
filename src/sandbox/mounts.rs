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
    let (_, mounted_rootfs_path) = validate_mount_paths(metadata)?;
    if !is_mountpoint(&mounted_rootfs_path)? {
        return Ok(());
    }

    let output = Command::new("umount")
        .arg(&mounted_rootfs_path)
        .output()
        .context("failed to run umount")?;
    if !output.status.success() {
        return Err(anyhow!(
            "failed to unmount sandbox rootfs ({}): {}",
            output.status,
            command_failure_detail(&output)
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
