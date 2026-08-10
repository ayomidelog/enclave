use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

pub(crate) fn marker_path(rootfs_path: &Path, digest: &str, index: u64) -> Result<PathBuf> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("setup digest must be a 64-character hexadecimal value");
    }
    Ok(rootfs_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("rootfs path has no sandbox parent"))?
        .join("runtime/setup-cache")
        .join(format!("{digest}-{index}.done")))
}

pub(crate) fn is_complete(rootfs_path: &Path, digest: &str, index: u64) -> Result<bool> {
    Ok(marker_path(rootfs_path, digest, index)?.is_file())
}

pub(crate) fn mark_complete(rootfs_path: &Path, digest: &str, index: u64) -> Result<()> {
    let marker = marker_path(rootfs_path, digest, index)?;
    let parent = marker
        .parent()
        .ok_or_else(|| anyhow::anyhow!("setup marker has no parent"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create setup cache {}", parent.display()))?;
    crate::fsutil::write_file_atomic(&marker, b"completed\n", 0o600)
}

#[cfg(test)]
#[path = "../../tests/src/sandbox/setup_cache.rs"]
mod tests;
