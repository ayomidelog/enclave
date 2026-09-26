//! Why a cached rootfs was or was not reused.

use std::path::Path;

use super::fingerprint::{fingerprint, has_required_dirs};
use super::index::read;

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
