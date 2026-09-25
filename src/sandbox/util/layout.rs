//! The shape of a sandbox directory.
//!
//! The paths are derived rather than stored, so a sandbox whose record
//! predates a path keeps working: normalization fills in what is missing and
//! the layout call creates it.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::sandbox::types::SandboxMetadata;

pub fn normalize_sandbox_metadata(metadata: &mut SandboxMetadata) {
    let sandbox_dir = PathBuf::from(&metadata.sandbox_path);
    if metadata.mounted_rootfs_path.is_empty() {
        metadata.mounted_rootfs_path = sandbox_dir
            .join("runtime")
            .join("rootfs.mnt")
            .to_string_lossy()
            .to_string();
    }
    if metadata.workspaces_path.is_empty() {
        metadata.workspaces_path = sandbox_dir.join("workspaces").to_string_lossy().to_string();
    }
    if metadata.home_base_path.is_empty() {
        metadata.home_base_path = sandbox_dir.join("home-base").to_string_lossy().to_string();
    }
}

pub fn ensure_sandbox_layout(metadata: &SandboxMetadata) -> Result<()> {
    fs::create_dir_all(&metadata.mounted_rootfs_path)
        .with_context(|| format!("failed to create {}", metadata.mounted_rootfs_path))?;
    fs::create_dir_all(&metadata.workspaces_path)
        .with_context(|| format!("failed to create {}", metadata.workspaces_path))?;
    fs::create_dir_all(&metadata.home_base_path)
        .with_context(|| format!("failed to create {}", metadata.home_base_path))?;
    Ok(())
}

pub fn effective_rootfs_path(metadata: &SandboxMetadata) -> String {
    if metadata.status.is_active() && !metadata.mounted_rootfs_path.is_empty() {
        return metadata.mounted_rootfs_path.clone();
    }
    metadata.rootfs_path.clone()
}

pub(crate) fn dir_size(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }

    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];

    while let Some(current) = stack.pop() {
        let metadata = fs::symlink_metadata(&current)
            .with_context(|| format!("failed to stat {}", current.display()))?;

        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
            continue;
        }

        if metadata.is_dir() {
            for entry in fs::read_dir(&current)
                .with_context(|| format!("failed to read {}", current.display()))?
            {
                stack.push(entry?.path());
            }
        }
    }

    Ok(total)
}
