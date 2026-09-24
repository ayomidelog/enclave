//! The registry: the durable record of which sandboxes and workspaces exist.

mod repair;
mod storage;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::sandbox::SandboxMetadata;
use crate::workspace::WorkspaceMetadata;

pub use repair::repair_registry;
pub(crate) use storage::{
    load_registry_unlocked, registry_lock_path, save_registry_unlocked, update_cache,
    with_cached_registry,
};

/// Schema version of `registry.json` written by this binary.
pub(crate) const REGISTRY_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registry {
    pub version: u32,
    #[serde(default)]
    pub generation: u64,
    pub sandboxes: BTreeMap<String, RegistrySandbox>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrySandbox {
    pub metadata: SandboxMetadata,
    pub workspaces: BTreeMap<String, WorkspaceMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepairReport {
    pub added_sandboxes: usize,
    pub removed_sandboxes: usize,
    pub added_workspaces: usize,
    pub removed_workspaces: usize,
    /// Sandbox and workspace records whose persisted lifecycle state had to be
    /// reconciled with what was actually running.
    #[serde(default)]
    pub reconciled_runtime_records: usize,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            version: REGISTRY_VERSION,
            generation: 0,
            sandboxes: BTreeMap::new(),
        }
    }
}

pub fn ensure_registry(state_dir: &Path) -> Result<()> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create state dir {}", state_dir.display()))?;

    let path = registry_path(state_dir);
    if path.exists() {
        return Ok(());
    }

    let lock_path = registry_lock_path(state_dir);
    crate::fsutil::with_file_lock(&lock_path, || {
        if path.exists() {
            return Ok(());
        }
        let payload = serde_json::to_vec(&Registry::default())?;
        crate::fsutil::write_file_atomic(&path, &payload, 0o600)
            .with_context(|| format!("failed to initialize registry {}", path.display()))?;
        Ok(())
    })?;
    Ok(())
}

pub fn registry_path(state_dir: &Path) -> PathBuf {
    state_dir.join("registry.json")
}

pub fn with_registry<T, F>(state_dir: &Path, operation: F) -> Result<T>
where
    F: Fn(&Registry) -> Result<T>,
{
    ensure_registry(state_dir)?;
    let lock_path = registry_lock_path(state_dir);
    let path = registry_path(state_dir);
    if let Some(result) = with_cached_registry(&path, &operation)? {
        return Ok(result);
    }

    crate::fsutil::with_file_lock(&lock_path, || {
        if let Some(result) = with_cached_registry(&path, &operation)? {
            return Ok(result);
        }
        let registry = load_registry_unlocked(state_dir)?;
        let result = operation(&registry)?;
        update_cache(state_dir, registry);
        Ok(result)
    })
}

pub fn with_registry_mut<T, F>(state_dir: &Path, operation: F) -> Result<T>
where
    F: FnOnce(&mut Registry) -> Result<T>,
{
    ensure_registry(state_dir)?;
    let lock_path = registry_lock_path(state_dir);
    crate::fsutil::with_file_lock(&lock_path, || {
        let mut registry = load_registry_unlocked(state_dir)?;
        let out = operation(&mut registry)?;
        registry.generation = registry.generation.saturating_add(1);
        save_registry_unlocked(state_dir, &registry)?;
        update_cache(state_dir, registry);
        Ok(out)
    })
}

#[cfg(test)]
#[path = "../../tests/src/registry.rs"]
mod tests;
