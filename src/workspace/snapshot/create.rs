use super::*;

pub fn create_workspace_snapshot(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    snapshot_name: Option<&str>,
) -> Result<WorkspaceSnapshotInfo> {
    let workspace = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_selector))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_selector))
    })?;

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
}
