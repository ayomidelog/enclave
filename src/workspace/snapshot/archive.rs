use super::*;

use crate::hostcmd::HostCommand;

use std::time::Duration;

// A snapshot archive can be large, so its transfer gets a longer deadline than
// the default while still being bounded.
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(600);

pub fn export_workspace_snapshot_archive(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    snapshot_name: &str,
    output: &Path,
) -> Result<WorkspaceSnapshotArchiveInfo> {
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
    validate_snapshot_name(snapshot_name)?;
    let snapshot_dir = snapshot_directory(&workspace, snapshot_name)?;
    ensure_snapshot_layout(&snapshot_dir, snapshot_name)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    export_snapshot_archive(&snapshot_dir, snapshot_name, output)?;
    Ok(WorkspaceSnapshotArchiveInfo {
        name: snapshot_name.to_string(),
        archive_path: output.to_string_lossy().to_string(),
    })
}

pub fn import_workspace_snapshot_archive(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    archive: &Path,
    snapshot_name: Option<&str>,
    replace: bool,
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

    let workspace_dir = PathBuf::from(&workspace.workspace_path);
    let workspace_dir =
        crate::fsutil::ensure_path_within(&workspace_dir, &workspace_dir, "workspace path")?;
    let snapshots_dir = workspace_dir.join("snapshots");
    fs::create_dir_all(&snapshots_dir)
        .with_context(|| format!("failed to create {}", snapshots_dir.display()))?;

    let temp_dir = temporary_workspace("snapshot-import");
    fs::create_dir_all(&temp_dir)
        .with_context(|| format!("failed to create {}", temp_dir.display()))?;

    let outcome = (|| {
        extract_archive(archive, &temp_dir)?;
        let extracted_root = locate_extracted_snapshot(&temp_dir)?;
        let metadata = read_snapshot_metadata(&extracted_root)?;
        let target_name = snapshot_name.unwrap_or(&metadata.name);
        validate_snapshot_name(target_name)?;

        let destination = snapshots_dir.join(target_name);
        if destination.exists() {
            if !replace {
                bail!(
                    "snapshot '{}' already exists for workspace '{}'; pass --replace to overwrite it",
                    target_name,
                    workspace.name
                );
            }
            fs::remove_dir_all(&destination)
                .with_context(|| format!("failed to remove {}", destination.display()))?;
        }
        copy_dir_recursive(&extracted_root, &destination)?;

        let final_metadata = SnapshotMetadata {
            name: target_name.to_string(),
            created_at: metadata.created_at,
        };
        let metadata_path = destination.join("snapshot.json");
        crate::fsutil::write_file_atomic(
            &metadata_path,
            serde_json::to_string_pretty(&final_metadata)?.as_bytes(),
            0o600,
        )
        .with_context(|| format!("failed to write {}", metadata_path.display()))?;

        Ok::<WorkspaceSnapshotInfo, anyhow::Error>(WorkspaceSnapshotInfo {
            name: final_metadata.name,
            created_at: final_metadata.created_at,
            path: destination.to_string_lossy().to_string(),
        })
    })();

    let _ = fs::remove_dir_all(&temp_dir);
    outcome
}

pub(crate) fn export_snapshot_archive(
    snapshot_dir: &Path,
    snapshot_name: &str,
    output: &Path,
) -> Result<()> {
    let parent = snapshot_dir.parent().ok_or_else(|| {
        anyhow!(
            "snapshot directory {} has no parent directory",
            snapshot_dir.display()
        )
    })?;
    let mut command = HostCommand::new("tar").arg("-C").arg(parent);
    if output_uses_gzip(output) {
        command = command.arg("-czf");
    } else {
        command = command.arg("-cf");
    }
    command
        .arg(output)
        .arg(snapshot_name)
        .timeout(ARCHIVE_TIMEOUT)
        .run_checked()
        .with_context(|| format!("failed to run tar for {}", snapshot_dir.display()))?;
    Ok(())
}

pub(crate) fn extract_archive(archive: &Path, target_dir: &Path) -> Result<()> {
    if !archive.is_file() {
        bail!(
            "archive {} does not exist or is not a regular file",
            archive.display()
        );
    }
    HostCommand::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(target_dir)
        .timeout(ARCHIVE_TIMEOUT)
        .run_checked()
        .with_context(|| format!("failed to run tar -xf {}", archive.display()))?;
    Ok(())
}

pub(crate) fn locate_extracted_snapshot(extract_dir: &Path) -> Result<PathBuf> {
    if looks_like_snapshot_dir(extract_dir) {
        return Ok(extract_dir.to_path_buf());
    }

    let mut candidates = Vec::new();
    for entry in fs::read_dir(extract_dir)
        .with_context(|| format!("failed to read {}", extract_dir.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() && looks_like_snapshot_dir(&entry.path()) {
            candidates.push(entry.path());
        }
    }

    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => bail!(
            "archive did not contain a recognizable snapshot layout under {}",
            extract_dir.display()
        ),
        _ => bail!(
            "archive contained multiple snapshot-like directories under {}; keep a single snapshot payload per archive",
            extract_dir.display()
        ),
    }
}

pub(crate) fn looks_like_snapshot_dir(path: &Path) -> bool {
    path.join("snapshot.json").is_file()
        && path.join("fs").is_dir()
        && path.join("home-upper").is_dir()
}

pub(crate) fn output_uses_gzip(path: &Path) -> bool {
    matches!(path.extension().and_then(OsStr::to_str), Some("gz" | "tgz"))
}
