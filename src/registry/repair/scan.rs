//! Reading the sandboxes tree and describing what is actually on disk.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::sandbox::{ensure_sandbox_layout, normalize_sandbox_metadata, SandboxMetadata};
use crate::workspace::WorkspaceMetadata;

use super::super::storage::{persist_metadata, read_json};
use super::super::RegistrySandbox;
use super::orphan::remove_orphan_directory;
use super::{DiskScan, RetainedOrphan};

pub(super) fn scan_on_disk(
    state_dir: &Path,
    strict: bool,
    known: &crate::registry::Registry,
) -> Result<DiskScan> {
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
        let known_sandbox = known.sandboxes.get(&dir_name);
        // A live create owns this directory until it commits its own record, so
        // the scan leaves it alone whether or not its metadata has been written
        // yet. The metadata is written before the registry insert, and a create
        // runs this repair, so without this a sibling create's repair would adopt
        // the record first and the create's own commit would fail with a name
        // that "already exists". A marker left by a create that died names a
        // process that is gone, so it does not protect the directory.
        //
        // A record the registry already has is never skipped: between the
        // create's registry commit and its marker removal the directory carries
        // both, and dropping it from the scan would delete the record it just
        // committed.
        if known_sandbox.is_none() && crate::fsutil::creation_in_progress(&sandbox_dir) {
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

        let discovered_workspaces =
            scan_workspaces(&metadata, strict, &mut scan.retained_orphans, known_sandbox)?;
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
    known: Option<&RegistrySandbox>,
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
        // Same as a sandbox: a live create owns its directory until it commits,
        // and its metadata is written before that commit. A workspace the
        // registry already records is never skipped, so the window between a
        // create's commit and its marker removal cannot delete the record.
        let known_workspace = known.is_some_and(|entry| entry.workspaces.contains_key(&dir_name));
        if !known_workspace && crate::fsutil::creation_in_progress(&workspace_dir) {
            continue;
        }
        if !metadata_path.exists() {
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
