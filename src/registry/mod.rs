//! The registry: the durable record of which sandboxes and workspaces exist.

mod migrate;
mod repair;
mod storage;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::sandbox::SandboxMetadata;
use crate::workspace::WorkspaceMetadata;

pub(crate) use migrate::{migrate, MigrationStep};
pub(crate) use repair::workspace_state_differences;
pub use repair::{repair_registry, RetainedOrphan};
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
    /// The schema version the registry was written with, when it was older than
    /// the one this binary writes and had to be migrated before use.
    #[serde(default)]
    pub migrated_registry_from_version: Option<u32>,
    /// Workspace directories repair refused to remove because a live runtime
    /// still owns them. Their metadata is gone, so this is the only place the
    /// operator learns that a runtime must be stopped before the directory can be
    /// cleaned up.
    #[serde(default)]
    pub retained_orphans: Vec<RetainedOrphan>,
    /// Records where the registry and the per-directory metadata disagreed about
    /// lifecycle state, and which copy repair adopted.
    ///
    /// Repair adopts the on-disk copy, and that rule is what this list keeps
    /// honest: without it the adoption is a silent overwrite, and an operator
    /// looking at a workspace whose state changed unexpectedly has no way to see
    /// that the two copies had diverged or which one won.
    #[serde(default)]
    pub metadata_disagreements: Vec<MetadataDisagreement>,
}

/// One registry record and its on-disk copy disagreeing about lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MetadataDisagreement {
    pub sandbox_id: String,
    /// The workspace the disagreement is about, or nothing for the sandbox's own
    /// record.
    pub workspace_id: Option<String>,
    /// Each field that differed, as `field: registry=<value> disk=<value>`.
    pub differences: Vec<String>,
    /// The copy repair adopted. The on-disk copy always wins, because a lifecycle
    /// step writes it before it commits the registry record.
    pub adopted: String,
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
#[path = "../../tests/src/registry/mod.rs"]
mod tests;
