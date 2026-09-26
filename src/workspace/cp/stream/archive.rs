use super::*;

pub(crate) fn write_host_directory_archive<W: Write>(
    source: &str,
    source_name: &str,
    writer: W,
) -> Result<(u64, u64)> {
    let mut archive = tar::Builder::new(writer);
    let source_path = Path::new(source);
    archive
        .append_dir(source_name, source_path)
        .with_context(|| format!("failed to archive host directory '{}'", source))?;

    let mut pending = vec![(source_path.to_path_buf(), PathBuf::from(source_name))];
    let mut logical_bytes = 0u64;
    let mut files = 0u64;
    while let Some((directory, archive_directory)) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("failed to read host source '{}'", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let archive_path = archive_directory.join(entry.file_name());
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("failed to stat host source '{}'", path.display()))?;
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                archive
                    .append_dir(&archive_path, &path)
                    .with_context(|| format!("failed to archive directory '{}'", path.display()))?;
                pending.push((path, archive_path));
            } else if file_type.is_file() || file_type.is_symlink() {
                if file_type.is_file() {
                    logical_bytes = logical_bytes.saturating_add(metadata.len());
                    files = files.saturating_add(1);
                }
                archive
                    .append_path_with_name(&path, &archive_path)
                    .with_context(|| {
                        format!("failed to archive host source '{}'", path.display())
                    })?;
            } else {
                bail!(
                    "refusing to archive unsupported host source type '{}'",
                    path.display()
                );
            }
        }
    }
    archive
        .finish()
        .context("failed to finish host directory archive")?;
    Ok((logical_bytes, files))
}

pub(crate) fn restore_workspace_metadata(
    workspace: &WorkspaceMetadata,
    path: &str,
    metadata: &fs::Metadata,
) -> Result<()> {
    let relative = Path::new(path)
        .strip_prefix("/home")
        .context("workspace staging path is outside /home")?;
    let base = workspace
        .home_mount_source_path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&workspace.filesystem_path));
    let target = base.join(relative);
    fs::set_permissions(
        &target,
        fs::Permissions::from_mode(metadata.permissions().mode()),
    )
    .with_context(|| format!("failed to restore workspace permissions {}", path))?;
    let accessed = metadata
        .accessed()
        .context("failed to read host source access time")?
        .duration_since(std::time::UNIX_EPOCH)
        .context("host source access time predates Unix epoch")?;
    let modified = metadata
        .modified()
        .context("failed to read host source modification time")?
        .duration_since(std::time::UNIX_EPOCH)
        .context("host source modification time predates Unix epoch")?;
    let target = CString::new(target.as_os_str().as_bytes())
        .context("workspace staging path contains an interior NUL byte")?;
    let times = [
        libc::timespec {
            tv_sec: accessed.as_secs() as libc::time_t,
            tv_nsec: accessed.subsec_nanos() as libc::c_long,
        },
        libc::timespec {
            tv_sec: modified.as_secs() as libc::time_t,
            tv_nsec: modified.subsec_nanos() as libc::c_long,
        },
    ];
    let result = unsafe { libc::utimensat(libc::AT_FDCWD, target.as_ptr(), times.as_ptr(), 0) };
    if result != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to restore workspace timestamp {}", path));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn extract_workspace_archive<R: Read>(
    stream: R,
    stage: &HostStagingDirectory,
    source_name: &str,
) -> Result<u64> {
    extract_workspace_archive_with_stats(stream, stage, source_name).map(|(bytes, _)| bytes)
}

pub(crate) fn extract_workspace_archive_with_stats<R: Read>(
    stream: R,
    stage: &HostStagingDirectory,
    source_name: &str,
) -> Result<(u64, u64)> {
    let mut archive = Archive::new(stream);
    let mut logical_bytes = 0u64;
    let mut files = 0u64;
    let mut entries = HashSet::new();
    for entry in archive
        .entries()
        .context("failed to read workspace tar entries")?
    {
        let mut entry = entry.context("failed to read workspace tar entry")?;
        let path = entry
            .path()
            .context("workspace tar entry has an invalid path")?
            .into_owned();
        validate_archive_path(&path, source_name)?;
        validate_archive_entry_type(entry.header().entry_type())?;
        if !entries.insert(path.clone()) {
            bail!(
                "workspace tar archive contains duplicate entry '{}'",
                path.display()
            );
        }
        ensure_no_symlink_ancestors(stage.path(), &path)?;
        if entry.header().entry_type().is_file() {
            logical_bytes = logical_bytes.saturating_add(entry.header().size()?);
            files = files.saturating_add(1);
        }
        entry.unpack_in(stage.path()).with_context(|| {
            format!("failed to unpack workspace tar entry '{}'", path.display())
        })?;
    }
    let root = stage.path().join(source_name);
    if fs::symlink_metadata(&root).is_err() {
        bail!("workspace tar archive did not contain expected entry '{source_name}'");
    }
    Ok((logical_bytes, files))
}

pub(crate) fn validate_archive_path(path: &Path, source_name: &str) -> Result<()> {
    let mut components = path.components();
    let Some(Component::Normal(root)) = components.next() else {
        bail!(
            "workspace tar archive contains unsafe entry '{}'",
            path.display()
        );
    };
    if root != source_name {
        bail!(
            "workspace tar archive contains unexpected entry '{}'; expected '{}...'",
            path.display(),
            source_name
        );
    }
    for component in components {
        if !matches!(component, Component::Normal(_)) {
            bail!(
                "workspace tar archive contains unsafe entry '{}'",
                path.display()
            );
        }
    }
    Ok(())
}

pub(crate) fn validate_archive_entry_type(entry_type: tar::EntryType) -> Result<()> {
    if entry_type.is_file() || entry_type.is_dir() || entry_type.is_symlink() {
        return Ok(());
    }
    bail!("workspace tar archive contains unsupported entry type")
}

pub(crate) fn ensure_no_symlink_ancestors(root: &Path, path: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in path.components() {
        let Component::Normal(component) = component else {
            bail!(
                "workspace tar archive contains unsafe entry '{}'",
                path.display()
            );
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "workspace tar archive writes through symlinked entry '{}'",
                    current.display()
                )
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to inspect staged entry {}", current.display())
                })
            }
        }
    }
    Ok(())
}

pub(crate) fn tar_create_args(source: &str, source_name: &str, gzip: bool) -> Vec<String> {
    let parent = Path::new(source)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("/"));
    vec![
        "-C".into(),
        parent.to_string_lossy().into_owned(),
        if gzip { "-czf" } else { "-cf" }.into(),
        "-".into(),
        "--".into(),
        source_name.into(),
    ]
}

pub(crate) fn tar_extract_args_at(destination: &str, gzip: bool) -> Vec<String> {
    vec![
        "-C".into(),
        destination.into(),
        if gzip { "-xzpf" } else { "-xpf" }.into(),
        "-".into(),
        "-o".into(),
        "--".into(),
    ]
}

pub(crate) fn workspace_tar_command(args: Vec<String>) -> Vec<String> {
    let mut command = vec!["tar".to_string()];
    command.extend(args);
    command
}
