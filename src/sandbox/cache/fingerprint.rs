//! Whether a cached rootfs directory is still the one that was registered.
//!
//! A cache hit is only safe if the content is the content that was measured, so
//! an entry carries both a cheap fingerprint for the fast check and a content
//! digest for the identity the setup cache key depends on.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::index::CacheFingerprint;

const REQUIRED_ROOTFS_DIRS: [&str; 3] = ["bin", "etc", "usr"];

pub(super) fn has_required_dirs(path: &Path) -> bool {
    REQUIRED_ROOTFS_DIRS
        .iter()
        .all(|name| path.join(name).is_dir())
}

pub(super) fn fingerprint(path: &Path) -> Result<CacheFingerprint> {
    let mut values = Vec::with_capacity(REQUIRED_ROOTFS_DIRS.len());
    for name in REQUIRED_ROOTFS_DIRS {
        let metadata = fs::metadata(path.join(name))?;
        values.push((
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
        ));
    }
    let (device, inode, size, modified_seconds, modified_nanos) = values.into_iter().fold(
        (0u64, 0u64, 0u64, 0i64, 0i64),
        |(device, inode, size, seconds, nanos),
         (next_device, next_inode, next_size, next_seconds, next_nanos)| {
            (
                device ^ next_device,
                inode ^ next_inode,
                size.saturating_add(next_size),
                seconds ^ next_seconds,
                nanos ^ next_nanos,
            )
        },
    );
    Ok(CacheFingerprint {
        device,
        inode,
        size,
        modified_seconds,
        modified_nanos,
    })
}

pub(super) fn content_digest(path: &Path) -> Result<String> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut paths = Vec::new();
    collect_paths(path, &mut paths)?;
    paths.sort();
    for child in paths {
        let relative = child
            .strip_prefix(path)
            .unwrap_or(&child)
            .to_string_lossy()
            .into_owned();
        relative.hash(&mut hasher);
        let metadata = fs::symlink_metadata(&child)?;
        metadata.len().hash(&mut hasher);
        metadata.mode().hash(&mut hasher);
        if metadata.file_type().is_file() {
            let mut file = fs::File::open(&child)?;
            let mut buffer = [0u8; 1024 * 1024];
            loop {
                let read = std::io::Read::read(&mut file, &mut buffer)?;
                if read == 0 {
                    break;
                }
                buffer[..read].hash(&mut hasher);
            }
        }
    }
    Ok(format!("{:016x}", hasher.finish()))
}

pub(super) fn collect_paths(path: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    paths.push(path.to_path_buf());
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        collect_paths(&entry?.path(), paths)?;
    }
    Ok(())
}
