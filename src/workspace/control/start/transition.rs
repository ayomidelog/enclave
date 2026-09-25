//! The registry transitions a start records.
//!
//! A launch takes tens of milliseconds and can fail partway, so the state it
//! leaves behind has to be readable: Starting before the runtime exists,
//! Running once its identity is committed, and back to Stopped with the partial
//! launch's host resources released when it fails.

use super::super::*;

/// Release the host resources a workspace runtime left behind when it died.
///
/// The registry still describes what the dead runtime owned, so the record is
/// the input: the interface name comes from its address and id, the cgroup from
/// its pid, and the mounts from its storage paths. The teardown is the same one
/// a stop runs, and it verifies itself, so a start that cannot release the old
/// resources fails instead of stacking a second runtime on top of them.
pub(super) fn release_dead_runtime_resources(
    state_dir: &std::path::Path,
    sandbox_id: &str,
    workspace_id: &str,
    workspace_snapshot: &WorkspaceMetadata,
) -> Result<()> {
    with_registry_mut(state_dir, |registry| {
        let sandbox = registry
            .sandboxes
            .get_mut(sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let latest = sandbox
            .workspaces
            .get(workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        // The snapshot was taken before any host work started, so the record has
        // to still describe the same runtime. A different one means a competing
        // operation already dealt with it, and this start should be retried
        // against the new state rather than release resources it does not own.
        if latest.runtime_pid != workspace_snapshot.runtime_pid
            || latest.runtime_starttime_ticks != workspace_snapshot.runtime_starttime_ticks
            || latest.assigned_ip != workspace_snapshot.assigned_ip
        {
            bail!(
                "workspace '{}' changed while its dead runtime was being released; retry",
                workspace_id
            );
        }
        set_workspace_stopped(sandbox, workspace_id).map(|_| ())
    })
    .with_context(|| {
        format!(
            "failed to release the resources of the dead runtime of workspace '{}'",
            workspace_id
        )
    })
}

/// Record that a workspace runtime launch has begun.
pub(crate) fn mark_workspace_starting(
    state_dir: &std::path::Path,
    sandbox_id: &str,
    workspace_id: &str,
) -> Result<String> {
    with_registry_mut(state_dir, |registry| {
        let used = collect_all_used_ip_octets(registry);
        let assigned_ip = network::ipam::allocate_ip(&used)?;
        let workspace = registry
            .sandboxes
            .get_mut(sandbox_id)
            .and_then(|sandbox| sandbox.workspaces.get_mut(workspace_id))
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        workspace.status = WorkspaceStatus::Starting;
        workspace.assigned_ip = Some(assigned_ip.clone());
        persist_workspace_metadata(workspace)?;
        Ok(assigned_ip)
    })
}

/// Roll a failed launch back to `Stopped`, tearing down anything the partial
/// launch left behind (workspace storage mounts, cgroups, network, `/tmp`).
pub(crate) fn mark_workspace_start_failed(
    state_dir: &std::path::Path,
    sandbox_id: &str,
    workspace_id: &str,
) -> Result<()> {
    with_registry_mut(state_dir, |registry| {
        let sandbox = registry
            .sandboxes
            .get_mut(sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        set_workspace_stopped(sandbox, workspace_id).map(|_| ())
    })
}
