//! Registry repair: reconcile `registry.json` with what is on disk.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::sandbox::{ensure_sandbox_layout, normalize_sandbox_metadata};
use crate::workspace::{OrphanRuntime, WorkspaceMetadata};

use crate::sandbox::SandboxMetadata;

use super::storage::{
    load_registry_with_migrations, persist_metadata, read_json, registry_lock_path,
    save_registry_unlocked, update_cache,
};
use super::{ensure_registry, Registry, RegistrySandbox, RepairReport};

/// A workspace directory repair refused to remove because a live runtime owns it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RetainedOrphan {
    pub sandbox_id: String,
    pub workspace_id: String,
    pub workspace_dir: PathBuf,
    pub runtime: OrphanRuntime,
}

impl RetainedOrphan {
    /// One line naming the directory and what still owns it.
    pub fn describe(&self) -> String {
        format!(
            "{}/{} at {} ({})",
            self.sandbox_id,
            self.workspace_id,
            self.workspace_dir.display(),
            self.runtime.describe()
        )
    }
}

/// What one scan of the sandboxes tree found.
#[derive(Default)]
struct DiskScan {
    sandboxes: BTreeMap<String, RegistrySandbox>,
    /// Workspace directories retained because a live runtime still owns them.
    retained_orphans: Vec<RetainedOrphan>,
}

pub fn repair_registry(state_dir: &Path, strict: bool) -> Result<RepairReport> {
    ensure_registry(state_dir)?;
    let lock_path = registry_lock_path(state_dir);
    crate::fsutil::with_file_lock(&lock_path, || {
        let (mut registry, migrations) = match load_registry_with_migrations(state_dir) {
            Ok(loaded) => loaded,
            Err(err) => {
                tracing::warn!(
                    "registry repair is rebuilding in-memory state after registry load failure: {err:#}"
                );
                (Registry::default(), Vec::new())
            }
        };
        let mut report = RepairReport {
            // The version the record was written with, so an operator can see
            // that repair understood an older schema rather than overwriting it.
            migrated_registry_from_version: migrations.first().map(|step| step.from),
            ..RepairReport::default()
        };

        let sandboxes_root = state_dir.join("sandboxes");
        fs::create_dir_all(&sandboxes_root)
            .with_context(|| format!("failed to create {}", sandboxes_root.display()))?;

        let DiskScan {
            sandboxes: discovered,
            retained_orphans,
        } = scan_on_disk(state_dir, strict)?;
        report.retained_orphans = retained_orphans;

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
        update_cache(state_dir, registry);
        Ok(report)
    })
}

fn scan_on_disk(state_dir: &Path, strict: bool) -> Result<DiskScan> {
    let sandboxes_root = state_dir.join("sandboxes");
    let mut scan = DiskScan::default();

    if !sandboxes_root.exists() {
        return Ok(scan);
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
            // A sandbox being created has no `sandbox.json` yet, because the
            // record is only written once the rootfs is in place. Removing it
            // would delete a sibling create's work in progress, so a directory a
            // live process has claimed is left alone. A marker left behind by a
            // create that died names a process that is gone, so it does not
            // protect the directory from being cleaned up.
            if crate::fsutil::creation_in_progress(&sandbox_dir) {
                continue;
            }
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

        let discovered_workspaces = scan_workspaces(&metadata, strict, &mut scan.retained_orphans)?;
        scan.sandboxes.insert(
            metadata.id.clone(),
            RegistrySandbox {
                metadata,
                workspaces: discovered_workspaces,
            },
        );
    }

    Ok(scan)
}

fn scan_workspaces(
    sandbox: &SandboxMetadata,
    strict: bool,
    retained_orphans: &mut Vec<RetainedOrphan>,
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
            // Same as a sandbox: a workspace being created has no metadata yet, so
            // a live create marker is what keeps a concurrent repair from deleting
            // it. A marker from a create that died does not protect the directory.
            if crate::fsutil::creation_in_progress(&workspace_dir) {
                continue;
            }
            // A workspace whose metadata is gone may still have a running runtime.
            // Deleting its directory would leave that runtime, its cgroup, its
            // interface, and its firewall rules owned by nothing, so the directory
            // is retained and reported instead. Discovery reads only the markers
            // the runtime itself wrote; it never signals a process.
            if let Some(orphan) =
                crate::workspace::find_orphan_runtime(&workspace_dir, &sandbox.id, &dir_name)
            {
                tracing::warn!(
                    "registry repair retained workspace directory {} with no metadata: a live runtime still owns it ({})",
                    workspace_dir.display(),
                    orphan.describe()
                );
                retained_orphans.push(RetainedOrphan {
                    workspace_dir: workspace_dir.clone(),
                    sandbox_id: sandbox.id.clone(),
                    workspace_id: dir_name.clone(),
                    runtime: orphan,
                });
                continue;
            }
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
