//! Where the rootfs cache lives.

use super::*;

pub(crate) fn rootfs_cache_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("sandboxes").join("rootfs-cache")
}

pub(crate) fn ensure_rootfs_cache(state_dir: &Path) -> Result<PathBuf> {
    let cache_dir = rootfs_cache_dir(state_dir);
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("failed to create rootfs cache at {}", cache_dir.display()))?;
    if !cache::index_path(&cache_dir).is_file() {
        cache::rebuild(&cache_dir)?;
    }
    Ok(cache_dir)
}
