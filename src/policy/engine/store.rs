//! Reading and writing the policy file.
//!
//! The file is the only durable record of the policy, so it is written atomically and
//! kept at mode 0600. Reads go through the cache in the sibling module, and the lock
//! is what makes a read-modify-write safe against a second writer.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::super::types::Policy;
use super::cache::{cached_policy, update_policy_cache};

pub fn ensure_policy(state_dir: &Path) -> Result<()> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create state dir {}", state_dir.display()))?;
    let path = policy_path(state_dir);
    if path.exists() {
        ensure_policy_permissions(&path)?;
        return Ok(());
    }
    let lock_path = policy_lock_path(state_dir);
    crate::fsutil::with_file_lock(&lock_path, || {
        if path.exists() {
            ensure_policy_permissions(&path)?;
            return Ok(());
        }
        save_policy_unlocked(state_dir, &Policy::default())?;
        ensure_policy_permissions(&path)?;
        Ok(())
    })
}

pub fn load_policy(state_dir: &Path) -> Result<Policy> {
    with_policy(state_dir, |policy| Ok(policy.clone()))
}

pub(super) fn policy_path(state_dir: &Path) -> PathBuf {
    state_dir.join("policy.json")
}

pub(super) fn with_policy<T, F>(state_dir: &Path, operation: F) -> Result<T>
where
    F: FnOnce(&Policy) -> Result<T>,
{
    ensure_policy(state_dir)?;
    let path = policy_path(state_dir);
    if let Some(policy) = cached_policy(&path)? {
        return operation(&policy);
    }
    let lock_path = policy_lock_path(state_dir);
    crate::fsutil::with_file_lock(&lock_path, || {
        let policy = load_policy_unlocked(state_dir)?;
        update_policy_cache(&path, &policy)?;
        operation(&policy)
    })
}

pub(super) fn with_policy_mut<T, F>(state_dir: &Path, operation: F) -> Result<T>
where
    F: FnOnce(&mut Policy) -> Result<T>,
{
    ensure_policy(state_dir)?;
    let lock_path = policy_lock_path(state_dir);
    crate::fsutil::with_file_lock(&lock_path, || {
        let mut policy = load_policy_unlocked(state_dir)?;
        let result = operation(&mut policy)?;
        save_policy_unlocked(state_dir, &policy)?;
        update_policy_cache(&policy_path(state_dir), &policy)?;
        Ok(result)
    })
}

fn load_policy_unlocked(state_dir: &Path) -> Result<Policy> {
    let path = policy_path(state_dir);
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read policy {}", path.display()))?;
    let policy: Policy =
        serde_json::from_str(&raw).with_context(|| format!("invalid policy {}", path.display()))?;
    Ok(policy)
}

fn save_policy_unlocked(state_dir: &Path, policy: &Policy) -> Result<()> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create state dir {}", state_dir.display()))?;
    let path = policy_path(state_dir);
    let raw = serde_json::to_string_pretty(policy)?;
    crate::fsutil::write_file_atomic(&path, raw.as_bytes(), 0o600)
        .with_context(|| format!("failed to write policy {}", path.display()))?;
    ensure_policy_permissions(&path)?;
    Ok(())
}

fn ensure_policy_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .permissions();
    let mode = perms.mode() & 0o777;
    if mode != 0o600 {
        perms.set_mode(0o600);
        fs::set_permissions(path, perms)
            .with_context(|| format!("failed to set mode on {}", path.display()))?;
    }
    Ok(())
}

fn policy_lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join("policy.lock")
}
