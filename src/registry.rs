use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::sandbox::{ensure_sandbox_layout, normalize_sandbox_metadata, SandboxMetadata};
use crate::workspace::WorkspaceMetadata;

const REGISTRY_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registry {
    pub version: u32,
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
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            version: REGISTRY_VERSION,
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
        let payload = serde_json::to_string_pretty(&Registry::default())?;
        crate::fsutil::write_file_atomic(&path, payload.as_bytes(), 0o600)
            .with_context(|| format!("failed to initialize registry {}", path.display()))?;
        Ok(())
    })?;
    Ok(())
}

pub fn registry_path(state_dir: &Path) -> PathBuf {
    state_dir.join("registry.json")
}

pub fn repair_registry(state_dir: &Path, strict: bool) -> Result<RepairReport> {
    with_registry_mut(state_dir, |registry| {
        let mut report = RepairReport::default();

        let sandboxes_root = state_dir.join("sandboxes");
        fs::create_dir_all(&sandboxes_root)
            .with_context(|| format!("failed to create {}", sandboxes_root.display()))?;

        let discovered = scan_on_disk(state_dir, strict)?;

        for (sandbox_id, discovered_sandbox) in &discovered {
            match registry.sandboxes.get_mut(sandbox_id) {
                Some(existing) => {
                    existing.metadata = discovered_sandbox.metadata.clone();

                    for (workspace_id, workspace) in &discovered_sandbox.workspaces {
                        if !existing.workspaces.contains_key(workspace_id) {
                            report.added_workspaces += 1;
                        }
                        existing
                            .workspaces
                            .insert(workspace_id.clone(), workspace.clone());
                    }

                    if strict {
                        let stale_ids: Vec<String> = existing
                            .workspaces
                            .keys()
                            .filter(|id| !discovered_sandbox.workspaces.contains_key(*id))
                            .cloned()
                            .collect();
                        for workspace_id in stale_ids {
                            existing.workspaces.remove(&workspace_id);
                            report.removed_workspaces += 1;
                        }
                    }
                }
                None => {
                    report.added_sandboxes += 1;
                    report.added_workspaces += discovered_sandbox.workspaces.len();
                    registry
                        .sandboxes
                        .insert(sandbox_id.clone(), discovered_sandbox.clone());
                }
            }
        }

        if strict {
            let stale_sandbox_ids: Vec<String> = registry
                .sandboxes
                .keys()
                .filter(|id| !discovered.contains_key(*id))
                .cloned()
                .collect();
            for sandbox_id in stale_sandbox_ids {
                if let Some(removed) = registry.sandboxes.remove(&sandbox_id) {
                    report.removed_sandboxes += 1;
                    report.removed_workspaces += removed.workspaces.len();
                }
            }
        }

        Ok(report)
    })
}

pub fn with_registry<T, F>(state_dir: &Path, operation: F) -> Result<T>
where
    F: FnOnce(&Registry) -> Result<T>,
{
    ensure_registry(state_dir)?;
    let lock_path = registry_lock_path(state_dir);
    crate::fsutil::with_file_lock(&lock_path, || {
        let registry = load_registry_unlocked(state_dir)?;
        operation(&registry)
    })
}

pub fn with_registry_mut<T, F>(state_dir: &Path, operation: F) -> Result<T>
where
    F: FnOnce(&mut Registry) -> Result<T>,
{
    ensure_registry(state_dir)?;
    let lock_path = registry_lock_path(state_dir);
    crate::fsutil::with_file_lock(&lock_path, || {
        let path = registry_path(state_dir);
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read registry {}", path.display()))?;
        let mut registry = match serde_json::from_str::<Registry>(&raw) {
            Ok(registry) => registry,
            Err(err) => {
                tracing::warn!(
                    "registry parse failed; recovering with empty registry at {}: {err:#}",
                    path.display()
                );
                Registry::default()
            }
        };
        let out = operation(&mut registry)?;
        save_registry_unlocked(state_dir, &registry)?;
        Ok(out)
    })
}

fn scan_on_disk(state_dir: &Path, strict: bool) -> Result<BTreeMap<String, RegistrySandbox>> {
    let sandboxes_root = state_dir.join("sandboxes");
    let mut result = BTreeMap::new();

    if !sandboxes_root.exists() {
        return Ok(result);
    }

    for entry in fs::read_dir(&sandboxes_root)
        .with_context(|| format!("failed to read {}", sandboxes_root.display()))?
    {
        let entry = entry?;
        let sandbox_dir = entry.path();
        if !sandbox_dir.is_dir() {
            continue;
        }

        let dir_name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(name) => {
                if strict {
                    bail!(
                        "strict repair failed: sandbox directory name is not valid utf-8: {:?}",
                        name
                    );
                }
                tracing::warn!(
                    "registry repair skipped non-utf8 sandbox directory: {:?}",
                    name
                );
                continue;
            }
        };
        if dir_name == "rootfs-cache" {
            continue;
        }
        let metadata_path = sandbox_dir.join("sandbox.json");
        if !metadata_path.exists() {
            if strict {
                bail!(
                    "strict repair failed: missing sandbox metadata {}",
                    metadata_path.display()
                );
            }
            continue;
        }

        let mut metadata: SandboxMetadata = match read_json(&metadata_path) {
            Ok(metadata) => metadata,
            Err(err) => {
                if strict {
                    return Err(err);
                }
                tracing::warn!("registry repair skipped invalid sandbox metadata: {err:#}");
                continue;
            }
        };

        if metadata.id.is_empty() {
            if strict {
                bail!(
                    "strict repair failed: sandbox metadata has empty id at {}",
                    metadata_path.display()
                );
            }
            metadata.id = dir_name.clone();
        } else if metadata.id != dir_name {
            if strict {
                bail!(
                    "strict repair failed: sandbox id '{}' does not match dir '{}'",
                    metadata.id,
                    dir_name
                );
            }
            metadata.id = dir_name.clone();
        }

        if metadata.sandbox_path.is_empty() {
            metadata.sandbox_path = sandbox_dir.to_string_lossy().to_string();
        }
        if Path::new(&metadata.sandbox_path) != sandbox_dir {
            if strict {
                bail!(
                    "strict repair failed: sandbox '{}' path mismatch (metadata={}, disk={})",
                    metadata.id,
                    metadata.sandbox_path,
                    sandbox_dir.display()
                );
            }
            metadata.sandbox_path = sandbox_dir.to_string_lossy().to_string();
        }

        normalize_sandbox_metadata(&mut metadata);
        ensure_sandbox_layout(&metadata)?;

        let discovered_workspaces = scan_workspaces(&metadata, strict)?;
        result.insert(
            metadata.id.clone(),
            RegistrySandbox {
                metadata,
                workspaces: discovered_workspaces,
            },
        );
    }

    Ok(result)
}

fn scan_workspaces(
    sandbox: &SandboxMetadata,
    strict: bool,
) -> Result<BTreeMap<String, WorkspaceMetadata>> {
    let mut result = BTreeMap::new();
    let workspaces_path = PathBuf::from(&sandbox.workspaces_path);
    fs::create_dir_all(&workspaces_path)
        .with_context(|| format!("failed to create {}", workspaces_path.display()))?;

    for entry in fs::read_dir(&workspaces_path)
        .with_context(|| format!("failed to read {}", workspaces_path.display()))?
    {
        let entry = entry?;
        let workspace_dir = entry.path();
        if !workspace_dir.is_dir() {
            continue;
        }

        let dir_name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(name) => {
                if strict {
                    bail!(
                        "strict repair failed: workspace directory name is not valid utf-8: {:?}",
                        name
                    );
                }
                tracing::warn!(
                    "registry repair skipped non-utf8 workspace directory: {:?}",
                    name
                );
                continue;
            }
        };
        let metadata_path = workspace_dir.join("workspace.json");
        if !metadata_path.exists() {
            if strict {
                bail!(
                    "strict repair failed: missing workspace metadata {}",
                    metadata_path.display()
                );
            }
            continue;
        }

        let mut metadata: WorkspaceMetadata = match read_json(&metadata_path) {
            Ok(metadata) => metadata,
            Err(err) => {
                if strict {
                    return Err(err);
                }
                tracing::warn!("registry repair skipped invalid workspace metadata: {err:#}");
                continue;
            }
        };

        if metadata.id.is_empty() {
            if strict {
                bail!(
                    "strict repair failed: workspace metadata has empty id at {}",
                    metadata_path.display()
                );
            }
            metadata.id = dir_name.clone();
        } else if metadata.id != dir_name {
            if strict {
                bail!(
                    "strict repair failed: workspace id '{}' does not match dir '{}'",
                    metadata.id,
                    dir_name
                );
            }
            metadata.id = dir_name.clone();
        }

        if metadata.sandbox_id != sandbox.id {
            if strict {
                bail!(
                    "strict repair failed: workspace '{}' sandbox mismatch (metadata={}, expected={})",
                    metadata.id,
                    metadata.sandbox_id,
                    sandbox.id
                );
            }
            metadata.sandbox_id = sandbox.id.clone();
        }

        if metadata.workspace_path.is_empty() {
            metadata.workspace_path = workspace_dir.to_string_lossy().to_string();
        }
        if Path::new(&metadata.workspace_path) != workspace_dir {
            if strict {
                bail!(
                    "strict repair failed: workspace '{}' path mismatch (metadata={}, disk={})",
                    metadata.id,
                    metadata.workspace_path,
                    workspace_dir.display()
                );
            }
            metadata.workspace_path = workspace_dir.to_string_lossy().to_string();
        }

        result.insert(metadata.id.clone(), metadata);
    }

    Ok(result)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read json {}", path.display()))?;
    let parsed = serde_json::from_str(&raw)
        .with_context(|| format!("invalid json metadata {}", path.display()))?;
    Ok(parsed)
}

fn load_registry_unlocked(state_dir: &Path) -> Result<Registry> {
    let path = registry_path(state_dir);
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read registry {}", path.display()))?;
    let registry: Registry = serde_json::from_str(&raw)
        .with_context(|| format!("invalid registry {}", path.display()))?;
    Ok(registry)
}

fn save_registry_unlocked(state_dir: &Path, registry: &Registry) -> Result<()> {
    let path = registry_path(state_dir);
    let payload = serde_json::to_string_pretty(registry)?;
    crate::fsutil::write_file_atomic(&path, payload.as_bytes(), 0o600)
        .with_context(|| format!("failed to write registry {}", path.display()))?;
    Ok(())
}

fn registry_lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join("registry.lock")
}

#[cfg(test)]
#[path = "../tests/src/registry.rs"]
mod tests;
