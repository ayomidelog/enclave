use super::*;

pub(crate) fn set_workspace_stopped(
    sandbox: &mut RegistrySandbox,
    workspace_id: &str,
) -> Result<()> {
    let workspace = sandbox
        .workspaces
        .get(workspace_id)
        .cloned()
        .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
    cleanup::run_workspace_stop_cleanup(
        WorkspaceStopCleanup {
            sandbox: sandbox.metadata.clone(),
            workspace,
        },
        false,
        false,
    )?;
    mark_workspace_stopped(sandbox, workspace_id)?;
    remove_sandbox_cgroup_if_idle(sandbox);
    Ok(())
}

pub(crate) fn mark_workspace_stopped(
    sandbox: &mut RegistrySandbox,
    workspace_id: &str,
) -> Result<()> {
    let workspace = sandbox
        .workspaces
        .get_mut(workspace_id)
        .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
    workspace.status = WorkspaceStatus::Stopped;
    workspace.runtime_pid = None;
    workspace.runtime_starttime_ticks = None;
    workspace.assigned_ip = None;
    clear_workspace_namespace_refs(workspace);
    let pid_file = session::runtime_pid_file(workspace);
    if let Err(err) = fs::remove_file(&pid_file) {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("failed to remove {}: {err:#}", pid_file.display());
        }
    }
    let ready_file = session::runtime_ready_file(workspace);
    if let Err(err) = fs::remove_file(&ready_file) {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("failed to remove {}: {err:#}", ready_file.display());
        }
    }

    let metadata_path = PathBuf::from(&workspace.workspace_path).join("workspace.json");
    let metadata_raw = serde_json::to_string_pretty(workspace)?;
    crate::fsutil::write_file_atomic(&metadata_path, metadata_raw.as_bytes(), 0o600).with_context(
        || {
            format!(
                "failed to persist workspace state to {}",
                metadata_path.display()
            )
        },
    )?;
    Ok(())
}

pub(crate) fn normalize_namespace_ref_paths(workspace: &mut WorkspaceMetadata) {
    let (mount_ref_path, pid_ref_path) = session::namespace_ref_paths(workspace);
    workspace.namespace_refs.mount = mount_ref_path.to_string_lossy().to_string();
    workspace.namespace_refs.pid = pid_ref_path.to_string_lossy().to_string();
}

pub(crate) fn persist_workspace_metadata(workspace: &WorkspaceMetadata) -> Result<()> {
    let metadata_path = PathBuf::from(&workspace.workspace_path).join("workspace.json");
    let metadata_raw = serde_json::to_string_pretty(workspace)?;
    crate::fsutil::write_file_atomic(&metadata_path, metadata_raw.as_bytes(), 0o600).with_context(
        || {
            format!(
                "failed to persist workspace metadata to {}",
                metadata_path.display()
            )
        },
    )
}

pub(crate) fn clear_workspace_namespace_refs(workspace: &mut WorkspaceMetadata) {
    if let Err(err) = session::clear_namespace_ref_files(workspace) {
        tracing::warn!(
            "failed to clear namespace refs for workspace {}: {err:#}",
            workspace.id
        );
    }
    workspace.namespace_refs = Default::default();
}

pub(crate) fn remove_sandbox_cgroup_if_idle(sandbox: &RegistrySandbox) {
    if sandbox
        .workspaces
        .values()
        .all(|item| item.status != WorkspaceStatus::Running)
        && !sandbox.metadata.limits.has_limits()
    {
        remove_sandbox_cgroup(&sandbox.metadata);
    }
}

pub(crate) fn remove_sandbox_cgroup(sandbox: &SandboxMetadata) {
    let sandbox_path = std::path::PathBuf::from("/sys/fs/cgroup")
        .join(crate::sandbox::cgroup::sandbox_cgroup_name(&sandbox.id));
    if let Err(err) = crate::sandbox::cgroup::remove_cgroup_path(&sandbox_path) {
        tracing::debug!(
            "sandbox cgroup cleanup skipped for '{}': {err:#}",
            sandbox.id
        );
    }
}

pub(crate) fn resolve_workspace_id(sandbox: &RegistrySandbox, selector: &str) -> Result<String> {
    if sandbox.workspaces.contains_key(selector) {
        return Ok(selector.to_string());
    }

    let mut matches = Vec::new();
    for (id, workspace) in &sandbox.workspaces {
        if workspace.name == selector {
            matches.push(id.clone());
        }
    }

    match matches.len() {
        0 => bail!(
            "workspace '{}' not found in sandbox '{}'",
            selector,
            sandbox.metadata.id
        ),
        1 => Ok(matches.remove(0)),
        _ => bail!(
            "workspace name '{}' is ambiguous in sandbox '{}'; use id instead (matches: {})",
            selector,
            sandbox.metadata.id,
            matches.join(", ")
        ),
    }
}

pub(crate) fn reconcile_workspace_runtime_state(workspace: &mut WorkspaceMetadata) -> Result<bool> {
    if workspace.status != WorkspaceStatus::Running {
        let stale_runtime_state = workspace.runtime_pid.is_some()
            || workspace.runtime_starttime_ticks.is_some()
            || workspace.assigned_ip.is_some()
            || workspace.namespace_refs.mount != "unassigned"
            || workspace.namespace_refs.pid != "unassigned"
            || session::namespace_ref_files_exist(workspace);
        if !stale_runtime_state {
            return Ok(false);
        }
        workspace.runtime_pid = None;
        workspace.runtime_starttime_ticks = None;
        workspace.assigned_ip = None;
        clear_workspace_namespace_refs(workspace);
        persist_workspace_metadata(workspace)?;
        return Ok(true);
    }

    let runtime_is_live = workspace
        .runtime_pid
        .zip(workspace.runtime_starttime_ticks)
        .is_some_and(|(pid, starttime)| session::process_matches(pid, Some(starttime)));
    if !runtime_is_live {
        workspace.status = WorkspaceStatus::Stopped;
        workspace.runtime_pid = None;
        workspace.runtime_starttime_ticks = None;
        workspace.assigned_ip = None;
        clear_workspace_namespace_refs(workspace);
        persist_workspace_metadata(workspace)?;
        return Ok(true);
    }

    let pid = workspace.runtime_pid.expect("live runtime has a pid");
    if session::namespace_refs_match_runtime(workspace, pid) {
        return Ok(false);
    }

    let (mount_ns, pid_ns) = session::read_namespace_refs(pid)?;
    normalize_namespace_ref_paths(workspace);
    session::write_namespace_ref_values(workspace, &mount_ns, &pid_ns)?;
    persist_workspace_metadata(workspace)?;
    Ok(true)
}
