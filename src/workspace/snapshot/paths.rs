use super::*;

pub(crate) fn snapshots_root(workspace: &WorkspaceMetadata) -> PathBuf {
    PathBuf::from(&workspace.workspace_path).join("snapshots")
}

pub(crate) fn snapshot_path(workspace: &WorkspaceMetadata, snapshot_name: &str) -> PathBuf {
    snapshots_root(workspace).join(snapshot_name)
}

pub(crate) fn snapshot_directory(
    workspace: &WorkspaceMetadata,
    snapshot_name: &str,
) -> Result<PathBuf> {
    let workspace_dir = PathBuf::from(&workspace.workspace_path);
    let workspace_dir =
        crate::fsutil::ensure_path_within(&workspace_dir, &workspace_dir, "workspace path")?;
    let snapshot_dir = crate::fsutil::ensure_path_within(
        &workspace_dir,
        &snapshot_path(workspace, snapshot_name),
        "snapshot path",
    )?;
    Ok(snapshot_dir)
}

pub(crate) fn default_snapshot_name() -> String {
    format!("snap-{}", Utc::now().format("%Y%m%d%H%M%S"))
}

pub(crate) fn validate_snapshot_name(name: &str) -> Result<()> {
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

pub(crate) fn ensure_snapshot_layout(snapshot_dir: &Path, snapshot_name: &str) -> Result<()> {
    if !snapshot_dir.exists() {
        bail!("snapshot '{}' not found", snapshot_name);
    }
    let snapshot_fs = snapshot_dir.join("fs");
    let snapshot_home_upper = snapshot_dir.join("home-upper");
    let metadata_path = snapshot_dir.join("snapshot.json");
    if !snapshot_fs.exists() || !snapshot_home_upper.exists() || !metadata_path.exists() {
        bail!("snapshot '{}' is incomplete", snapshot_name);
    }
    Ok(())
}

pub(crate) fn read_snapshot_metadata(snapshot_dir: &Path) -> Result<SnapshotMetadata> {
    let metadata_path = snapshot_dir.join("snapshot.json");
    if !metadata_path.is_file() {
        bail!(
            "snapshot archive did not contain snapshot.json under {}",
            snapshot_dir.display()
        );
    }
    let raw = fs::read_to_string(&metadata_path)
        .with_context(|| format!("failed to read {}", metadata_path.display()))?;
    let metadata = serde_json::from_str::<SnapshotMetadata>(&raw)
        .with_context(|| format!("failed to parse {}", metadata_path.display()))?;
    validate_snapshot_name(&metadata.name)?;
    Ok(metadata)
}

pub(crate) fn temporary_workspace(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("enclave-{label}-{}-{}", std::process::id(), nanos))
}

pub(crate) fn reset_path(path: &Path) -> Result<()> {
    if let Err(err) = fs::remove_dir_all(path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            return Err(err).with_context(|| format!("failed to remove {}", path.display()));
        }
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    Ok(())
}

pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
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
            } else if file_type.is_file()
                && !crate::fsutil::reflink_copy_file(&src_path, &dst_path)?
                && !crate::fsutil::copy_file_range_file(&src_path, &dst_path)?
            {
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
