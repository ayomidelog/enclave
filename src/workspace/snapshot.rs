use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::registry::{with_registry, with_registry_mut};
use crate::sandbox::resolve_sandbox_id;

use super::control::{resolve_workspace_id, set_workspace_stopped};
use super::session;
use super::types::{WorkspaceMetadata, WorkspaceSnapshotInfo, WorkspaceStatus};

pub const DEFAULT_SNAPSHOT_KEEP: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotMetadata {
    name: String,
    created_at: String,
}

pub fn create_workspace_snapshot(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    snapshot_name: Option<&str>,
) -> Result<WorkspaceSnapshotInfo> {
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
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;

        let snapshot_name = snapshot_name
            .map(str::to_string)
            .unwrap_or_else(default_snapshot_name);
        validate_snapshot_name(&snapshot_name)?;
        let workspace_dir = PathBuf::from(&workspace.workspace_path);
        let workspace_dir =
            crate::fsutil::ensure_path_within(&workspace_dir, &workspace_dir, "workspace path")?;

        let snapshots_dir = workspace_dir.join("snapshots");
        fs::create_dir_all(&snapshots_dir)
            .with_context(|| format!("failed to create {}", snapshots_dir.display()))?;
        let snapshot_dir = crate::fsutil::ensure_path_within(
            &workspace_dir,
            &snapshot_path(&workspace, &snapshot_name),
            "snapshot path",
        )?;
        if snapshot_dir.exists() {
            bail!("snapshot '{}' already exists", snapshot_name);
        }
        fs::create_dir_all(&snapshot_dir)
            .with_context(|| format!("failed to create {}", snapshot_dir.display()))?;

        let snapshot_result = crate::workspace::with_workspace_storage_mounted(&workspace, || {
            let snapshot_fs = snapshot_dir.join("fs");
            let snapshot_home_upper = snapshot_dir.join("home-upper");
            let filesystem_path = crate::fsutil::ensure_path_within(
                &workspace_dir,
                Path::new(&workspace.filesystem_path),
                "workspace filesystem path",
            )?;
            let overlay_upper_path = crate::fsutil::ensure_path_within(
                &workspace_dir,
                Path::new(&workspace.overlay_home_upper_path),
                "workspace home upper path",
            )?;
            copy_dir_recursive(&filesystem_path, &snapshot_fs)?;
            copy_dir_recursive(&overlay_upper_path, &snapshot_home_upper)?;

            let metadata = SnapshotMetadata {
                name: snapshot_name.clone(),
                created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            };
            let metadata_path = snapshot_dir.join("snapshot.json");
            crate::fsutil::write_file_atomic(
                &metadata_path,
                serde_json::to_string_pretty(&metadata)?.as_bytes(),
                0o600,
            )
            .with_context(|| format!("failed to write {}", metadata_path.display()))?;

            Ok::<WorkspaceSnapshotInfo, anyhow::Error>(WorkspaceSnapshotInfo {
                name: metadata.name,
                created_at: metadata.created_at,
                path: snapshot_dir.to_string_lossy().to_string(),
            })
        });
        if let Err(err) = snapshot_result {
            if snapshot_dir.exists() {
                fs::remove_dir_all(&snapshot_dir).with_context(|| {
                    format!(
                        "failed to clean up partial snapshot directory {}",
                        snapshot_dir.display()
                    )
                })?;
            }
            return Err(err);
        }
        snapshot_result
    })
}

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
    sorted.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    let to_remove = sorted.split_off(keep);
    let mut removed = Vec::new();
    for snapshot in &to_remove {
        let path = PathBuf::from(&snapshot.path);
        if let Err(err) = fs::remove_dir_all(&path) {
            if err.kind() != std::io::ErrorKind::NotFound {
                return Err(err)
                    .with_context(|| format!("failed to remove snapshot {}", path.display()));
            }
        }
        removed.push(snapshot.clone());
    }

    Ok(removed)
}

pub fn restore_workspace_snapshot(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    snapshot_name: &str,
) -> Result<WorkspaceMetadata> {
    with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        validate_snapshot_name(snapshot_name)?;

        if workspace.status == WorkspaceStatus::Running {
            if let Some(pid) = workspace.runtime_pid {
                session::stop_session(pid, workspace.runtime_starttime_ticks)?;
            }
            set_workspace_stopped(sandbox, &workspace_id)?;
        }

        restore_snapshot_filesystem(&workspace, snapshot_name)?;

        let result = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        Ok(result)
    })
}

fn snapshots_root(workspace: &WorkspaceMetadata) -> PathBuf {
    PathBuf::from(&workspace.workspace_path).join("snapshots")
}

fn snapshot_path(workspace: &WorkspaceMetadata, snapshot_name: &str) -> PathBuf {
    snapshots_root(workspace).join(snapshot_name)
}

fn default_snapshot_name() -> String {
    format!("snap-{}", Utc::now().format("%Y%m%d%H%M%S"))
}

fn validate_snapshot_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 63 {
        bail!("snapshot name must be 1-63 characters");
    }
    if name == "." || name == ".." || name.contains("..") {
        bail!("snapshot name must not contain '.' path traversal segments");
    }
    for c in name.chars() {
        if !(c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            bail!("snapshot name contains invalid character '{}'", c);
        }
    }
    Ok(())
}

fn reset_path(path: &Path) -> Result<()> {
    if let Err(err) = fs::remove_dir_all(path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            return Err(err).with_context(|| format!("failed to remove {}", path.display()));
        }
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !src.exists() {
        fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
        return Ok(());
    }

    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((current_src, current_dst)) = stack.pop() {
        fs::create_dir_all(&current_dst)
            .with_context(|| format!("failed to create {}", current_dst.display()))?;

        for entry in fs::read_dir(&current_src)
            .with_context(|| format!("failed to read {}", current_src.display()))?
        {
            let entry = entry?;
            let src_path = entry.path();
            let dst_path = current_dst.join(entry.file_name());
            let file_type = entry
                .file_type()
                .with_context(|| format!("failed to stat {}", src_path.display()))?;
            if file_type.is_symlink() {
                bail!("refusing to copy symlink {}", src_path.display());
            } else if file_type.is_dir() {
                stack.push((src_path, dst_path));
            } else if file_type.is_file() {
                fs::copy(&src_path, &dst_path).with_context(|| {
                    format!(
                        "failed to copy file {} -> {}",
                        src_path.display(),
                        dst_path.display()
                    )
                })?;
            }
        }
    }

    Ok(())
}

fn restore_snapshot_filesystem(workspace: &WorkspaceMetadata, snapshot_name: &str) -> Result<()> {
    crate::workspace::with_workspace_storage_mounted(workspace, || {
        let workspace_path = PathBuf::from(&workspace.workspace_path);
        let workspace_path =
            crate::fsutil::ensure_path_within(&workspace_path, &workspace_path, "workspace path")?;
        let snapshot_dir = crate::fsutil::ensure_path_within(
            &workspace_path,
            &snapshot_path(workspace, snapshot_name),
            "snapshot path",
        )?;
        if !snapshot_dir.exists() {
            bail!("snapshot '{}' not found", snapshot_name);
        }
        let snapshot_fs = snapshot_dir.join("fs");
        let snapshot_home_upper = snapshot_dir.join("home-upper");
        if !snapshot_fs.exists() || !snapshot_home_upper.exists() {
            bail!("snapshot '{}' is incomplete", snapshot_name);
        }

        let backup_root = workspace_path.join(".restore-backup");
        if backup_root.exists() {
            fs::remove_dir_all(&backup_root)
                .with_context(|| format!("failed to clean {}", backup_root.display()))?;
        }
        fs::create_dir_all(&backup_root)
            .with_context(|| format!("failed to create {}", backup_root.display()))?;

        let fs_path = crate::fsutil::ensure_path_within(
            &workspace_path,
            Path::new(&workspace.filesystem_path),
            "workspace filesystem path",
        )?;
        let upper_path = crate::fsutil::ensure_path_within(
            &workspace_path,
            Path::new(&workspace.overlay_home_upper_path),
            "workspace home upper path",
        )?;
        let work_path = crate::fsutil::ensure_path_within(
            &workspace_path,
            Path::new(&workspace.overlay_home_work_path),
            "workspace home work path",
        )?;
        let merged_path = crate::fsutil::ensure_path_within(
            &workspace_path,
            Path::new(&workspace.overlay_home_merged_path),
            "workspace home merged path",
        )?;
        let backup_fs = backup_root.join("fs");
        let backup_upper = backup_root.join("home-upper");

        if fs_path.exists() {
            fs::rename(&fs_path, &backup_fs).with_context(|| {
                format!(
                    "failed to move {} to backup {}",
                    fs_path.display(),
                    backup_fs.display()
                )
            })?;
        }
        if upper_path.exists() {
            fs::rename(&upper_path, &backup_upper).with_context(|| {
                format!(
                    "failed to move {} to backup {}",
                    upper_path.display(),
                    backup_upper.display()
                )
            })?;
        }
        if work_path.exists() {
            fs::remove_dir_all(&work_path)
                .with_context(|| format!("failed to remove {}", work_path.display()))?;
        }
        if merged_path.exists() {
            fs::remove_dir_all(&merged_path)
                .with_context(|| format!("failed to remove {}", merged_path.display()))?;
        }

        let restore_result = (|| {
            reset_path(&fs_path)?;
            reset_path(&upper_path)?;
            reset_path(&work_path)?;
            reset_path(&merged_path)?;
            copy_dir_recursive(&snapshot_fs, &fs_path)?;
            copy_dir_recursive(&snapshot_home_upper, &upper_path)?;
            Ok::<(), anyhow::Error>(())
        })();
        if let Err(err) = restore_result {
            if fs_path.exists() {
                if let Err(remove_err) = fs::remove_dir_all(&fs_path) {
                    tracing::warn!("failed to remove {}: {remove_err:#}", fs_path.display());
                }
            }
            if upper_path.exists() {
                if let Err(remove_err) = fs::remove_dir_all(&upper_path) {
                    tracing::warn!("failed to remove {}: {remove_err:#}", upper_path.display());
                }
            }
            if backup_fs.exists() {
                fs::rename(&backup_fs, &fs_path).with_context(|| {
                    format!(
                        "failed to rollback backup {} -> {}",
                        backup_fs.display(),
                        fs_path.display()
                    )
                })?;
            }
            if backup_upper.exists() {
                fs::rename(&backup_upper, &upper_path).with_context(|| {
                    format!(
                        "failed to rollback backup {} -> {}",
                        backup_upper.display(),
                        upper_path.display()
                    )
                })?;
            }
            return Err(err);
        }

        if backup_root.exists() {
            fs::remove_dir_all(&backup_root)
                .with_context(|| format!("failed to clean {}", backup_root.display()))?;
        }
        Ok(())
    })
}

#[cfg(test)]
#[path = "../../tests/src/workspace/snapshot.rs"]
mod tests;
