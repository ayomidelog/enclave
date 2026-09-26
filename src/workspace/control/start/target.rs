//! Working out which workspace a start is for, and what it is now.
//!
//! A start is given selectors rather than ids, and it takes a snapshot of the record
//! before it touches the host so the record can be compared against the snapshot at
//! each commit. This module is that half: resolving the target and reading its state.

use super::*;

pub(super) fn resolve_start_target(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<(String, String, SandboxMetadata, WorkspaceMetadata)> {
    with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        if sandbox.metadata.status != SandboxStatus::Running {
            bail!(
                "sandbox '{}' is stopped; start sandbox first",
                sandbox.metadata.id
            );
        }
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        if sandbox.metadata.status.is_transitional() {
            bail!(
                "sandbox '{}' is {:?}; wait for it to settle before starting workspaces",
                sandbox.metadata.id,
                sandbox.metadata.status
            );
        }
        let mut workspace_snapshot = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        if workspace_snapshot.status.is_transitional() {
            bail!(
                "workspace '{}' is {:?}; wait for the current operation to finish",
                workspace_id,
                workspace_snapshot.status
            );
        }
        workspace_snapshot.sandbox_rootfs_path = effective_rootfs_path(&sandbox.metadata);
        Ok((
            sandbox_id,
            workspace_id,
            sandbox.metadata.clone(),
            workspace_snapshot,
        ))
    })
}

/// Refresh the namespace references of a runtime that is genuinely still alive.
///
/// `None` means the process the record names is gone, so the caller has to treat
/// the record as a dead runtime instead.
pub(super) fn refresh_live_runtime(
    state_dir: &std::path::Path,
    sandbox_id: &str,
    workspace_snapshot: &WorkspaceMetadata,
) -> Result<Option<WorkspaceMetadata>> {
    let Some((pid, starttime)) = workspace_snapshot
        .runtime_pid
        .zip(workspace_snapshot.runtime_starttime_ticks)
    else {
        return Ok(None);
    };
    if !session::process_matches(pid, Some(starttime)) {
        return Ok(None);
    }
    let (mount_ns, pid_ns) = session::read_namespace_refs(pid)?;
    with_registry_mut(state_dir, |registry| {
        let sandbox = registry
            .sandboxes
            .get_mut(sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace = sandbox
            .workspaces
            .get_mut(&workspace_snapshot.id)
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_snapshot.id))?;
        if workspace.runtime_pid != Some(pid)
            || workspace.runtime_starttime_ticks != Some(starttime)
        {
            bail!("workspace runtime identity changed while refreshing namespace refs");
        }
        normalize_namespace_ref_paths(workspace);
        session::write_namespace_ref_values(workspace, &mount_ns, &pid_ns)?;
        Ok(Some(workspace.clone()))
    })
}

/// Release what a dead runtime still owns, then describe the workspace as
/// stopped so the launch below starts from a clean record.
///
/// The kernel releases the interface when the network namespace dies with the
/// process, but the anti-spoofing rules that name it, the cgroup, and the
/// storage mounts all outlive it. They are released the way a stop would, rather
/// than leaked to a later `doctor --repair`: a plain `workspace start` after a
/// crash is the command an operator runs first, and it must not leave the host
/// dirtier than it found it.
pub(super) fn release_dead_runtime(
    state_dir: &std::path::Path,
    sandbox_id: &str,
    workspace_id: &str,
    workspace_snapshot: &mut WorkspaceMetadata,
) -> Result<()> {
    transition::release_dead_runtime_resources(
        state_dir,
        sandbox_id,
        workspace_id,
        workspace_snapshot,
    )?;
    workspace_snapshot.status = WorkspaceStatus::Stopped;
    workspace_snapshot.runtime_pid = None;
    workspace_snapshot.runtime_starttime_ticks = None;
    workspace_snapshot.assigned_ip = None;
    Ok(())
}
