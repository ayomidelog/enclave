use super::*;

pub fn list_workspace_snapshots(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<Vec<WorkspaceSnapshotInfo>> {
    with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get(&workspace_id)
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;

        let mut out = Vec::new();
        let workspace_dir = PathBuf::from(&workspace.workspace_path);
        let workspace_dir =
            crate::fsutil::ensure_path_within(&workspace_dir, &workspace_dir, "workspace path")?;
        let snapshots_root = crate::fsutil::ensure_path_within(
            &workspace_dir,
            &snapshots_root(workspace),
            "snapshots root",
        )?;
        if !snapshots_root.exists() {
            return Ok(out);
        }

        for entry in fs::read_dir(&snapshots_root)
            .with_context(|| format!("failed to read {}", snapshots_root.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("failed to stat {}", path.display()))?;
            if file_type.is_symlink() || !file_type.is_dir() {
                continue;
            }
            let metadata_path = path.join("snapshot.json");
            let snapshot = if metadata_path.exists() {
                let raw = fs::read_to_string(&metadata_path)
                    .with_context(|| format!("failed to read {}", metadata_path.display()))?;
                serde_json::from_str::<SnapshotMetadata>(&raw)
                    .with_context(|| format!("failed to parse {}", metadata_path.display()))?
            } else {
                SnapshotMetadata {
                    name: entry.file_name().to_string_lossy().to_string(),
                    created_at: "<unknown>".to_string(),
                }
            };

            out.push(WorkspaceSnapshotInfo {
                name: snapshot.name,
                created_at: snapshot.created_at,
                path: path.to_string_lossy().to_string(),
            });
        }

        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    })
}

pub fn gc_workspace_snapshots(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    keep: usize,
) -> Result<Vec<WorkspaceSnapshotInfo>> {
    let all = list_workspace_snapshots(state_dir, sandbox_selector, workspace_selector)?;
    if all.len() <= keep {
        return Ok(Vec::new());
    }

    let mut sorted = all;
    newest_first(&mut sorted);

    let to_remove = sorted.split_off(keep);
    let worker_count = to_remove.len().min(4);
    let chunk_size = to_remove.len().div_ceil(worker_count);
    let chunks = to_remove
        .chunks(chunk_size)
        .map(|chunk| chunk.to_vec())
        .collect::<Vec<_>>();
    let results = thread::scope(|scope| {
        let handles = chunks
            .into_iter()
            .map(|chunk| {
                scope.spawn(move || {
                    let mut removed = Vec::with_capacity(chunk.len());
                    for snapshot in chunk {
                        let path = PathBuf::from(&snapshot.path);
                        if let Err(error) = fs::remove_dir_all(&path) {
                            if error.kind() != std::io::ErrorKind::NotFound {
                                return Err(error).with_context(|| {
                                    format!("failed to remove snapshot {}", path.display())
                                });
                            }
                        }
                        removed.push(snapshot);
                    }
                    Ok::<_, anyhow::Error>(removed)
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| anyhow!("snapshot garbage collection worker panicked"))?
            })
            .collect::<Result<Vec<_>>>()
    })?;
    Ok(results.into_iter().flatten().collect())
}

/// Order snapshots newest first.
///
/// The name breaks a tie on creation time so the choice is deterministic for two
/// snapshots that record the same time, which an imported archive can: it keeps
/// the creation time it was exported with rather than the time it was imported. A
/// default name embeds the creation time, so the name order agrees with the time
/// order for everything Enclave creates itself.
pub(crate) fn newest_first(snapshots: &mut [WorkspaceSnapshotInfo]) {
    snapshots.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.name.cmp(&a.name))
    });
}
