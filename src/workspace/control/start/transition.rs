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

/// Write the workspace's own record of a runtime that has just launched.
///
/// This runs before the registry commit and outside the registry lock, and both
/// halves of that are deliberate.
///
/// The precedence rule is that a lifecycle step writes the per-directory metadata
/// before it commits the registry record, so a crash between the two leaves a
/// record that is ahead of the registry rather than behind it. Writing the file
/// first is therefore the order the rule already asks for.
///
/// The lock is not what protects these files. A start holds the workspace's
/// lifecycle lease for its whole duration, so no other operation can be writing
/// them; the registry lock protects the registry. Writing three files here is six
/// fsyncs, which measured as most of the commit phase on this host, and none of it
/// has any business inside a lock every other lifecycle request has to take.
///
/// The caller still verifies under the lock that the workspace is the one this
/// launch was for. If it is not, the launch rolls back and rewrites this record as
/// stopped, so the file never disagrees with the registry for longer than the
/// rollback takes.
pub(crate) fn persist_started_workspace_runtime(
    workspace: &WorkspaceMetadata,
    started: &WorkspaceRuntimeStart,
) -> Result<WorkspaceMetadata> {
    let mut record = workspace.clone();
    record.status = WorkspaceStatus::Running;
    record.runtime_pid = Some(started.pid);
    record.runtime_starttime_ticks = Some(started.starttime_ticks);
    record.assigned_ip = Some(started.assigned_ip.clone());
    normalize_namespace_ref_paths(&mut record);
    session::write_namespace_ref_values(&record, &started.mount_ns, &started.pid_ns)?;
    persist_workspace_metadata(&record)?;
    Ok(record)
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
