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

mod verdict;

// The tests assert on the verdict by name, so it is reachable from the module
// root as well as from the check that returns it.
pub(crate) use verdict::evaluate;
#[cfg(test)]
pub(crate) use verdict::CacheVerdict;

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
#[path = "../../../tests/src/sandbox/cache.rs"]
mod tests;
