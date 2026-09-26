//! The rootfs cache index.
//!
//! A cached rootfs is a directory of tens of thousands of small files, so
//! copying it per sandbox is metadata bound. The cache lets a sandbox mount the
//! directory as an immutable lower layer instead, which means the index has to
//! answer two questions: is this directory still the rootfs that was registered,
//! and is it safe to reuse. The record and its storage live in the index module,
//! the answers in the fingerprint module, and this module is the API over both.

mod fingerprint;
mod index;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use fingerprint::{content_digest, fingerprint, has_required_dirs};
use index::{persist, read, CacheEntry, CacheIndex};

pub(crate) use index::index_path;

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
        return rebuild(cache_root);
    }
    adopt_unindexed_entries(cache_root)
}

/// Register rootfs directories that are present on disk but missing from the
/// index.
///
/// A cache directory can appear without the index being rebuilt: an operator
/// may extract a rootfs tarball directly into `rootfs-cache`, or an index may
/// be copied alongside only some of its entries. Ignoring those directories
/// makes a valid rootfs unusable for reasons the error message cannot explain.
fn adopt_unindexed_entries(cache_root: &Path) -> Result<()> {
    let mut index = read(cache_root)?;
    let mut adopted = 0usize;
    for entry in fs::read_dir(cache_root)
        .with_context(|| format!("failed to read rootfs cache {}", cache_root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() || !has_required_dirs(&path) {
            continue;
        }
        let key = entry.file_name().to_string_lossy().into_owned();
        if index
            .entries
            .iter()
            .any(|existing| existing.key == key && Path::new(&existing.path) == path)
        {
            continue;
        }
        tracing::info!(
            "rootfs cache: registering unindexed suite '{}' at {}",
            key,
            path.display()
        );
        index.entries.push(CacheEntry {
            key: key.clone(),
            path: path.to_string_lossy().into_owned(),
            fingerprint: fingerprint(&path)?,
            suite: key,
            architecture: std::env::consts::ARCH.to_string(),
            source: path.to_string_lossy().into_owned(),
            created_at: chrono::Utc::now().to_rfc3339(),
            content_digest: content_digest(&path)?,
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
        });
        adopted += 1;
    }
    if adopted > 0 {
        index.version = 1;
        index
            .entries
            .sort_by(|left, right| left.key.cmp(&right.key));
        persist(cache_root, &index)?;
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

/// Why a cached rootfs was or was not reused.
///
/// The question an operator asks when a sandbox copies a rootfs it expected to
/// share is which of the checks failed, and each one means something different:
/// a missing entry is a cache that was never built, a path mismatch is one that
/// moved, a missing directory is a cache that was deleted or never finished, and
/// a changed fingerprint is a cache whose contents were edited underneath it.
/// Reporting only that the cache missed would leave that question unanswerable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheVerdict {
    /// The entry is registered and its contents still match what was recorded.
    Hit,
    /// The index could not be read at all.
    IndexUnreadable,
    /// The index has no entry for this key and path.
    NotRegistered,
    /// The entry exists but its directory is gone or incomplete.
    MissingRequiredDirectories,
    /// The entry exists but its contents differ from the recorded fingerprint.
    ContentsChanged,
}

impl CacheVerdict {
    pub(crate) fn is_hit(self) -> bool {
        matches!(self, Self::Hit)
    }

    /// One phrase naming what the check found, for a log line or a report.
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::Hit => "the cache entry is registered and its contents are unchanged",
            Self::IndexUnreadable => "the cache index could not be read",
            Self::NotRegistered => "the cache index has no entry for this rootfs",
            Self::MissingRequiredDirectories => {
                "the cached rootfs directory is missing or incomplete"
            }
            Self::ContentsChanged => {
                "the cached rootfs contents differ from what the index recorded"
            }
        }
    }
}

/// Whether a cached rootfs can be reused, and why when it cannot.
pub(crate) fn evaluate(cache_root: &Path, key: &str, path: &Path) -> CacheVerdict {
    let verdict = match read(cache_root) {
        Ok(index) => match index
            .entries
            .iter()
            .find(|entry| entry.key == key && Path::new(&entry.path) == path)
        {
            None => CacheVerdict::NotRegistered,
            Some(entry) => {
                if !has_required_dirs(path) {
                    CacheVerdict::MissingRequiredDirectories
                } else if fingerprint(path).is_ok_and(|current| current == entry.fingerprint) {
                    CacheVerdict::Hit
                } else {
                    CacheVerdict::ContentsChanged
                }
            }
        },
        Err(_) => CacheVerdict::IndexUnreadable,
    };
    if verdict.is_hit() {
        crate::perf::record_cache_hit();
    } else {
        crate::perf::record_cache_miss();
    }
    verdict
}

/// The content identity recorded for a cache entry at the given path, when the
/// index knows one.
///
/// The digest is computed once when the entry is registered, so a caller that
/// only needs to know whether the content changed can compare digests instead of
/// walking the tree.
pub(crate) fn content_identity(cache_root: &Path, path: &Path) -> Option<String> {
    let index = read(cache_root).ok()?;
    index
        .entries
        .iter()
        .find(|entry| Path::new(&entry.path) == path)
        .map(|entry| entry.content_digest.clone())
        .filter(|digest| !digest.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{ensure, evaluate, index_path, rebuild, register, CacheVerdict};
    use std::fs;
    use std::path::PathBuf;

    /// Whether a cached rootfs can be reused. The tests assert on the reason as
    /// well, so this goes through the same check the daemon does.
    fn contains(cache_root: &std::path::Path, key: &str, path: &std::path::Path) -> bool {
        evaluate(cache_root, key, path).is_hit()
    }

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

    #[test]
    fn ensure_adopts_directories_missing_from_the_index() {
        let cache_root = temporary_cache();
        // Build an index that only knows about one suite, then add a second
        // rootfs directory the way an operator extracting a tarball would.
        rebuild(&cache_root).unwrap();
        let manual = cache_root.join("manual");
        for directory in ["bin", "etc", "usr"] {
            fs::create_dir_all(manual.join(directory)).unwrap();
        }
        assert!(!contains(&cache_root, "manual", &manual));

        ensure(&cache_root).unwrap();

        assert!(contains(&cache_root, "manual", &manual));
        assert!(contains(&cache_root, "suite", &cache_root.join("suite")));
        let _ = fs::remove_dir_all(cache_root);
    }
    /// Every way a cache can miss is reported as itself.
    ///
    /// The question an operator asks when a sandbox copies a rootfs they expected
    /// it to share is which check failed, and each answer means something
    /// different: a cache that was never built, one that was moved, one whose
    /// directory was deleted, or one whose contents were edited underneath it.
    /// Collapsing them into one "cache miss" would leave that question
    /// unanswerable.
    #[test]
    fn every_cache_verdict_names_what_it_found() {
        let cache_root = temporary_cache();
        let cache_path = cache_root.join("suite");

        // An index that is readable but holds no entry for this rootfs is the
        // shape a cache has before anything was ever registered in it.
        fs::write(index_path(&cache_root), br#"{"version":1,"entries":[]}"#).unwrap();
        assert_eq!(
            evaluate(&cache_root, "suite", &cache_path),
            CacheVerdict::NotRegistered
        );

        rebuild(&cache_root).unwrap();
        assert_eq!(
            evaluate(&cache_root, "suite", &cache_path),
            CacheVerdict::Hit
        );

        // A directory the fingerprint covers is removed, which is the shape a
        // half-finished extraction leaves behind.
        fs::remove_dir_all(cache_path.join("usr")).unwrap();
        assert_eq!(
            evaluate(&cache_root, "suite", &cache_path),
            CacheVerdict::MissingRequiredDirectories
        );

        // Put the directory back and change a file instead, so the required
        // directories are all present and only the contents disagree.
        for directory in ["bin", "etc", "usr"] {
            fs::create_dir_all(cache_path.join(directory)).unwrap();
        }
        fs::write(cache_path.join("etc").join("os-release"), b"edited").unwrap();
        assert_eq!(
            evaluate(&cache_root, "suite", &cache_path),
            CacheVerdict::ContentsChanged
        );

        // An index that cannot be parsed is its own answer rather than a miss
        // that looks like an empty cache.
        fs::write(index_path(&cache_root), b"not json").unwrap();
        assert_eq!(
            evaluate(&cache_root, "suite", &cache_path),
            CacheVerdict::IndexUnreadable
        );

        // Every verdict is a phrase rather than an empty string, so the log line
        // and any report that carries one is readable.
        for verdict in [
            CacheVerdict::Hit,
            CacheVerdict::IndexUnreadable,
            CacheVerdict::NotRegistered,
            CacheVerdict::MissingRequiredDirectories,
            CacheVerdict::ContentsChanged,
        ] {
            assert!(verdict.describe().len() > 20, "{verdict:?}");
        }

        let _ = fs::remove_dir_all(cache_root);
    }
}
