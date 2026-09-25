//! The policy file's fingerprint cache.
//!
//! The policy is read on every authorized request, and the file only changes when an
//! operator changes it. The cache holds the parsed policy alongside the file identity
//! it was parsed from, so a read costs one `stat` instead of a parse, and a change
//! written by anything else is noticed rather than missed.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};

use super::super::types::Policy;

#[derive(Clone)]
pub(super) struct PolicyCacheEntry {
    path: PathBuf,
    fingerprint: PolicyFingerprint,
    policy: Policy,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct PolicyFingerprint {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanos: i64,
}

static POLICY_CACHE: OnceLock<Mutex<Option<PolicyCacheEntry>>> = OnceLock::new();

pub(super) fn cached_policy(path: &Path) -> Result<Option<Policy>> {
    let fingerprint = policy_fingerprint(path)?;
    let cache = POLICY_CACHE.get_or_init(|| Mutex::new(None));
    let Ok(cache) = cache.lock() else {
        return Ok(None);
    };
    Ok(cache.as_ref().and_then(|entry| {
        (entry.path == path && entry.fingerprint == fingerprint).then(|| entry.policy.clone())
    }))
}

pub(super) fn update_policy_cache(path: &Path, policy: &Policy) -> Result<()> {
    let entry = PolicyCacheEntry {
        path: path.to_path_buf(),
        fingerprint: policy_fingerprint(path)?,
        policy: policy.clone(),
    };
    if let Ok(mut cache) = POLICY_CACHE.get_or_init(|| Mutex::new(None)).lock() {
        *cache = Some(entry);
    }
    Ok(())
}

pub(super) fn policy_fingerprint(path: &Path) -> Result<PolicyFingerprint> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to stat policy {}", path.display()))?;
    Ok(PolicyFingerprint {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanos: metadata.mtime_nsec(),
    })
}
