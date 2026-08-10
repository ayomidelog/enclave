use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::fsutil::write_file_atomic;

const INDEX_NAME: &str = "index.json";
const REQUIRED_ROOTFS_DIRS: [&str; 3] = ["bin", "etc", "usr"];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CacheIndex {
    version: u32,
    entries: Vec<CacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    key: String,
    path: String,
    fingerprint: CacheFingerprint,
    #[serde(default)]
    suite: String,
    #[serde(default)]
    architecture: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    content_digest: String,
    #[serde(default)]
    tool_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CacheFingerprint {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanos: i64,
}

pub(crate) fn index_path(cache_root: &Path) -> PathBuf {
    cache_root.join(INDEX_NAME)
}

pub(crate) fn rebuild(cache_root: &Path) -> Result<()> {
    let mut index = CacheIndex {
        version: 1,
        entries: Vec::new(),
    };
    if cache_root.is_dir() {
        for entry in fs::read_dir(cache_root)
            .with_context(|| format!("failed to read rootfs cache {}", cache_root.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_dir() || !has_required_dirs(&path) {
                continue;
            }
            let key = entry.file_name().to_string_lossy().into_owned();
            index.entries.push(CacheEntry {
                key,
                path: path.to_string_lossy().into_owned(),
                fingerprint: fingerprint(&path)?,
                suite: entry.file_name().to_string_lossy().into_owned(),
                architecture: std::env::consts::ARCH.to_string(),
                source: path.to_string_lossy().into_owned(),
                created_at: chrono::Utc::now().to_rfc3339(),
                content_digest: content_digest(&path)?,
                tool_version: env!("CARGO_PKG_VERSION").to_string(),
            });
        }
    }
    index
        .entries
        .sort_by(|left, right| left.key.cmp(&right.key));
    persist(cache_root, &index)
}

pub(crate) fn ensure(cache_root: &Path) -> Result<()> {
    if !index_path(cache_root).is_file() {
        rebuild(cache_root)?;
    }
    Ok(())
}

pub(crate) fn register(cache_root: &Path, key: &str, path: &Path) -> Result<()> {
    let mut index = read(cache_root).unwrap_or_default();
    index.version = 1;
    index.entries.retain(|entry| entry.key != key);
    if has_required_dirs(path) {
        index.entries.push(CacheEntry {
            key: key.to_string(),
            path: path.to_string_lossy().into_owned(),
            fingerprint: fingerprint(path)?,
            suite: key.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            source: path.to_string_lossy().into_owned(),
            created_at: chrono::Utc::now().to_rfc3339(),
            content_digest: content_digest(path)?,
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
        });
    }
    index
        .entries
        .sort_by(|left, right| left.key.cmp(&right.key));
    persist(cache_root, &index)
}

pub(crate) fn contains(cache_root: &Path, key: &str, path: &Path) -> bool {
    let Ok(index) = read(cache_root) else {
        crate::perf::record_cache_miss();
        return false;
    };
    let Some(entry) = index
        .entries
        .iter()
        .find(|entry| entry.key == key && Path::new(&entry.path) == path)
    else {
        crate::perf::record_cache_miss();
        return false;
    };
    let valid = has_required_dirs(path)
        && fingerprint(path).is_ok_and(|current| current == entry.fingerprint);
    if valid {
        crate::perf::record_cache_hit();
    } else {
        crate::perf::record_cache_miss();
    }
    valid
}

fn read(cache_root: &Path) -> Result<CacheIndex> {
    let raw = fs::read(index_path(cache_root))?;
    Ok(serde_json::from_slice(&raw)?)
}

fn persist(cache_root: &Path, index: &CacheIndex) -> Result<()> {
    let raw = serde_json::to_vec(index).context("failed to serialize rootfs cache index")?;
    write_file_atomic(&index_path(cache_root), &raw, 0o600).with_context(|| {
        format!(
            "failed to write rootfs cache index {}",
            index_path(cache_root).display()
        )
    })
}

fn has_required_dirs(path: &Path) -> bool {
    REQUIRED_ROOTFS_DIRS
        .iter()
        .all(|name| path.join(name).is_dir())
}

fn fingerprint(path: &Path) -> Result<CacheFingerprint> {
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

fn content_digest(path: &Path) -> Result<String> {
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

fn collect_paths(path: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::{contains, index_path, rebuild, register};
    use std::fs;
    use std::path::PathBuf;

    fn temporary_cache() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "enclave-rootfs-cache-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(path.join("suite").join("bin")).unwrap();
        fs::create_dir_all(path.join("suite").join("etc")).unwrap();
        fs::create_dir_all(path.join("suite").join("usr")).unwrap();
        path
    }

    #[test]
    fn index_tracks_and_invalidates_cache_fingerprint() {
        let cache_root = temporary_cache();
        let cache_path = cache_root.join("suite");
        rebuild(&cache_root).unwrap();
        assert!(contains(&cache_root, "suite", &cache_path));
        fs::remove_dir_all(cache_path.join("usr")).unwrap();
        assert!(!contains(&cache_root, "suite", &cache_path));
        let _ = fs::remove_dir_all(cache_root);
    }

    #[test]
    fn register_replaces_existing_key() {
        let cache_root = temporary_cache();
        let first_path = cache_root.join("suite");
        let second_path = cache_root.join("other");
        for directory in ["bin", "etc", "usr"] {
            fs::create_dir_all(second_path.join(directory)).unwrap();
        }
        rebuild(&cache_root).unwrap();
        register(&cache_root, "suite", &second_path).unwrap();
        assert!(!contains(&cache_root, "suite", &first_path));
        assert!(contains(&cache_root, "suite", &second_path));
        let _ = fs::remove_dir_all(cache_root);
    }

    #[test]
    fn index_records_provenance_and_content_digest() {
        let cache_root = temporary_cache();
        rebuild(&cache_root).unwrap();
        let raw = fs::read(index_path(&cache_root)).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        let entry = &json["entries"][0];
        assert_eq!(entry["suite"], "suite");
        assert_eq!(entry["architecture"], std::env::consts::ARCH);
        assert!(!entry["source"].as_str().unwrap().is_empty());
        assert!(!entry["created_at"].as_str().unwrap().is_empty());
        assert_eq!(entry["content_digest"].as_str().unwrap().len(), 16);
        assert_eq!(entry["tool_version"], env!("CARGO_PKG_VERSION"));
        let _ = fs::remove_dir_all(cache_root);
    }
}
