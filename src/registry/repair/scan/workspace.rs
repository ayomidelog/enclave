//! Describing the workspaces a sandbox directory holds.
//!
//! A workspace directory is the same problem as a sandbox directory one level
//! down, with one difference that matters: a workspace whose metadata is gone may
//! still have a running runtime, so its directory is retained and reported rather
//! than removed.

use super::*;

pub(super) fn scan_workspaces(
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
