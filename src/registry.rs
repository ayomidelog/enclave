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
    ensure_registry(state_dir)?;
    let lock_path = registry_lock_path(state_dir);
    crate::fsutil::with_file_lock(&lock_path, || {
        let mut registry = match load_registry_unlocked(state_dir) {
            Ok(registry) => registry,
            Err(err) => {
                tracing::warn!(
                    "registry repair is rebuilding in-memory state after registry load failure: {err:#}"
                );
                Registry::default()
            }
        };
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
                None => {
                    report.added_sandboxes += 1;
                    report.added_workspaces += discovered_sandbox.workspaces.len();
                    registry
                        .sandboxes
                        .insert(sandbox_id.clone(), discovered_sandbox.clone());
                }
            }
        }

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

        save_registry_unlocked(state_dir, &registry)?;
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
        let mut registry = load_registry_unlocked(state_dir)?;
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
            remove_orphan_directory(&sandbox_dir)?;
            continue;
        }

        let mut metadata: SandboxMetadata = match read_json(&metadata_path) {
            Ok(metadata) => metadata,
            Err(err) => {
                if strict {
                    return Err(err);
                }
                tracing::warn!(
                    "registry repair removed sandbox with invalid metadata {}: {err:#}",
                    metadata_path.display()
                );
                remove_orphan_directory(&sandbox_dir)?;
                continue;
            }
        };
        let original_metadata = serde_json::to_vec(&metadata)?;

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
        let rootfs_path = PathBuf::from(&metadata.rootfs_path);
        let rootfs_path =
            crate::fsutil::ensure_path_within(&sandbox_dir, &rootfs_path, "rootfs path")?;
        if !rootfs_path.is_dir() {
            if strict {
                bail!(
                    "strict repair failed: missing sandbox rootfs {}",
                    rootfs_path.display()
                );
            }
            tracing::warn!(
                "registry repair removed sandbox directory with missing rootfs {}",
                sandbox_dir.display()
            );
            remove_orphan_directory(&sandbox_dir)?;
            continue;
        }
        ensure_sandbox_layout(&metadata)?;

        if !strict && original_metadata != serde_json::to_vec(&metadata)? {
            persist_metadata(&metadata_path, &metadata)?;
        }

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
            remove_orphan_directory(&workspace_dir)?;
            continue;
        }

        let mut metadata: WorkspaceMetadata = match read_json(&metadata_path) {
            Ok(metadata) => metadata,
            Err(err) => {
                if strict {
                    return Err(err);
                }
                tracing::warn!(
                    "registry repair removed workspace with invalid metadata {}: {err:#}",
                    metadata_path.display()
                );
                remove_orphan_directory(&workspace_dir)?;
                continue;
            }
        };
        let original_metadata = serde_json::to_vec(&metadata)?;

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

        if !strict && original_metadata != serde_json::to_vec(&metadata)? {
            persist_metadata(&metadata_path, &metadata)?;
        }

        result.insert(metadata.id.clone(), metadata);
    }

    Ok(result)
}

fn remove_orphan_directory(path: &Path) -> Result<()> {
    if path_contains_mount(path)? {
        bail!(
            "refusing to remove orphaned directory {} while it or a descendant is still mounted",
            path.display()
        );
    }
    fs::remove_dir_all(path)
        .with_context(|| format!("failed to remove orphaned directory {}", path.display()))
}

fn path_contains_mount(path: &Path) -> Result<bool> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")
        .context("failed to read /proc/self/mountinfo")?;
    Ok(mountinfo
        .lines()
        .filter_map(mountinfo_path)
        .any(|mountpoint| mountpoint == path || mountpoint.starts_with(path)))
}

fn mountinfo_path(line: &str) -> Option<PathBuf> {
    let raw = line.split_whitespace().nth(4)?;
    Some(PathBuf::from(unescape_mountinfo_path(raw)))
}

fn unescape_mountinfo_path(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..=index + 3].iter().all(u8::is_ascii_digit)
        {
            let value = (bytes[index + 1] - b'0') * 64
                + (bytes[index + 2] - b'0') * 8
                + (bytes[index + 3] - b'0');
            result.push(value as char);
            index += 4;
        } else {
            result.push(bytes[index] as char);
            index += 1;
        }
    }
    result
}

fn persist_metadata<T: Serialize>(path: &Path, metadata: &T) -> Result<()> {
    let payload = serde_json::to_string_pretty(metadata)?;
    crate::fsutil::write_file_atomic(path, payload.as_bytes(), 0o600)
        .with_context(|| format!("failed to persist normalized metadata {}", path.display()))
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
