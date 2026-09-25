use super::*;

pub fn runtime_pid_file(workspace: &WorkspaceMetadata) -> PathBuf {
    runtime_dir(workspace).join("session.pid")
}

pub fn runtime_ready_file(workspace: &WorkspaceMetadata) -> PathBuf {
    runtime_dir(workspace).join("session.ready")
}

pub fn namespace_ref_paths(workspace: &WorkspaceMetadata) -> (PathBuf, PathBuf) {
    (
        resolve_namespace_ref_path(workspace, &workspace.namespace_refs.mount, "mnt.ref"),
        resolve_namespace_ref_path(workspace, &workspace.namespace_refs.pid, "pid.ref"),
    )
}

pub fn write_namespace_ref_values(
    workspace: &WorkspaceMetadata,
    mount_value: &str,
    pid_value: &str,
) -> Result<()> {
    let (mount_ref_path, pid_ref_path) = namespace_ref_paths(workspace);
    if let Some(parent) = mount_ref_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if let Some(parent) = pid_ref_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    // Best effort on purpose. These two files are the cross-check that a recorded
    // pid still has the namespaces it was recorded with, and they are re-derived
    // from the live runtime by the next reconcile whenever they are missing or
    // stale, so a lost write costs a re-derivation rather than a wrong answer.
    //
    // What they do have to survive is a *daemon* crash, because the runtime keeps
    // running and a later check has to tell it apart from a process that inherited
    // the pid. A write without fsync is already visible to every other process, so
    // it survives exactly that. It does not survive a power loss, and it does not
    // need to: a power loss takes the runtime with it, and the pid-reuse cross-check
    // has nothing left to be wrong about. That is why these two writes are the one
    // place on the start path where a durable write buys nothing; the workspace
    // record beside them stays durable because recovery reads it as the truth.
    crate::fsutil::write_file_atomic_with(
        &mount_ref_path,
        format!("{mount_value}\n").as_bytes(),
        0o600,
        crate::fsutil::Durability::BestEffort,
    )
    .with_context(|| format!("failed to write {}", mount_ref_path.display()))?;
    crate::fsutil::write_file_atomic_with(
        &pid_ref_path,
        format!("{pid_value}\n").as_bytes(),
        0o600,
        crate::fsutil::Durability::BestEffort,
    )
    .with_context(|| format!("failed to write {}", pid_ref_path.display()))?;
    Ok(())
}

pub fn namespace_refs_match_runtime(workspace: &WorkspaceMetadata, pid: u32) -> bool {
    let Ok((expected_mount, expected_pid)) = read_namespace_refs(pid) else {
        return false;
    };
    let (mount_ref_path, pid_ref_path) = namespace_ref_paths(workspace);
    let mount_ref = fs::read_to_string(mount_ref_path).ok();
    let pid_ref = fs::read_to_string(pid_ref_path).ok();
    mount_ref.is_some_and(|value| value.trim() == expected_mount)
        && pid_ref.is_some_and(|value| value.trim() == expected_pid)
}

pub fn clear_namespace_ref_files(workspace: &WorkspaceMetadata) -> Result<()> {
    let (mount_ref_path, pid_ref_path) = namespace_ref_paths(workspace);
    for path in [mount_ref_path, pid_ref_path] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to remove namespace ref {}", path.display()))
            }
        }
    }
    Ok(())
}

pub fn namespace_ref_files_exist(workspace: &WorkspaceMetadata) -> bool {
    let (mount_ref_path, pid_ref_path) = namespace_ref_paths(workspace);
    mount_ref_path.exists() || pid_ref_path.exists()
}

pub fn runtime_log_file(workspace: &WorkspaceMetadata) -> PathBuf {
    runtime_dir(workspace).join("session.log")
}

pub(crate) fn runtime_dir(workspace: &WorkspaceMetadata) -> PathBuf {
    PathBuf::from(&workspace.workspace_path).join("runtime")
}

pub(crate) fn sandbox_runtime_dir(workspace: &WorkspaceMetadata) -> PathBuf {
    PathBuf::from(&workspace.workspace_path)
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .map(|sandbox_dir| sandbox_dir.join("runtime"))
        .unwrap_or_else(|| runtime_dir(workspace))
}

pub(crate) fn ensure_runtime_layout(workspace: &WorkspaceMetadata) -> Result<()> {
    for path in [
        &workspace.workspace_path,
        &workspace.filesystem_path,
        &workspace.overlay_home_upper_path,
        &workspace.overlay_home_work_path,
        &workspace.overlay_home_merged_path,
    ] {
        fs::create_dir_all(path).with_context(|| format!("failed to create {}", path))?;
    }

    let (mount_ref_path, pid_ref_path) = namespace_ref_paths(workspace);
    if let Some(parent) = mount_ref_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if let Some(parent) = pid_ref_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::create_dir_all(runtime_dir(workspace))
        .with_context(|| format!("failed to create runtime dir for {}", workspace.id))?;
    fs::create_dir_all(sandbox_runtime_dir(workspace)).with_context(|| {
        format!(
            "failed to create sandbox runtime dir for {}",
            workspace.sandbox_id
        )
    })?;
    if workspace.home_mount_source_path.is_some() {
        return Ok(());
    }
    crate::workspace::create::ensure_traversable_directory_permissions(Path::new(
        &workspace.filesystem_path,
    ))?;
    Ok(())
}

pub(crate) fn resolve_namespace_ref_path(
    workspace: &WorkspaceMetadata,
    configured: &str,
    default_name: &str,
) -> PathBuf {
    let default_path = PathBuf::from(&workspace.workspace_path)
        .join("ns")
        .join(default_name);
    if configured.is_empty() || configured == "unassigned" {
        return default_path;
    }

    default_path
}
