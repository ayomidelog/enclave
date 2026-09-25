//! The cache index record and its storage.
//!
//! The index is one JSON file beside the cached rootfs directories. It is
//! written atomically, because a half-written index would make every entry it
//! describes look unregistered.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::fsutil::write_file_atomic;

const INDEX_NAME: &str = "index.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct CacheIndex {
    pub(super) version: u32,
    pub(super) entries: Vec<CacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CacheEntry {
    pub(super) key: String,
    pub(super) path: String,
    pub(super) fingerprint: CacheFingerprint,
    #[serde(default)]
    pub(super) suite: String,
    #[serde(default)]
    pub(super) architecture: String,
    #[serde(default)]
    pub(super) source: String,
    #[serde(default)]
    pub(super) created_at: String,
    #[serde(default)]
    pub(super) content_digest: String,
    #[serde(default)]
    pub(super) tool_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct CacheFingerprint {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) size: u64,
    pub(super) modified_seconds: i64,
    pub(super) modified_nanos: i64,
}

pub(crate) fn index_path(cache_root: &Path) -> PathBuf {
    cache_root.join(INDEX_NAME)
}

pub(super) fn read(cache_root: &Path) -> Result<CacheIndex> {
    let raw = fs::read(index_path(cache_root))?;
    Ok(serde_json::from_slice(&raw)?)
}

pub(super) fn persist(cache_root: &Path, index: &CacheIndex) -> Result<()> {
    let raw = serde_json::to_vec(index).context("failed to serialize rootfs cache index")?;
    write_file_atomic(&index_path(cache_root), &raw, 0o600).with_context(|| {
        format!(
            "failed to write rootfs cache index {}",
            index_path(cache_root).display()
        )
    })
}
