//! Stopping a workspace.
//!
//! A stop releases everything a workspace's runtime owns while keeping its files
//! and its registry record, and it proves the release with a certificate. There
//! are three entry points: one workspace, every workspace in a sandbox, and the
//! freeze a sandbox pause uses. The batch and freeze paths are separate modules
//! because each has its own ordering rules.

mod batch;
mod freeze;

use super::*;
use crate::workspace::WorkspaceCleanupCertificate;

pub(crate) use batch::stop_running_workspaces_in_sandbox;
pub(crate) use freeze::freeze_workspaces_in_sandbox;

pub fn stop_workspace(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<WorkspaceMetadata> {
    stop_workspace_with_certificate(state_dir, sandbox_selector, workspace_selector)
        .map(|(metadata, _)| metadata)
}

/// Stop a workspace and return the verified cleanup certificate alongside it.
///
/// Callers that own host resources outside the workspace layer (the port
/// publisher, for example) can attach their own verification to the certificate
/// before reporting the stop as complete.
pub fn stop_workspace_with_certificate(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<(WorkspaceMetadata, WorkspaceCleanupCertificate)> {
    let (sandbox_id, workspace_id, current, sandbox_metadata) =
        with_registry(state_dir, |registry| {
            let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
            let sandbox = registry
                .sandboxes
                .get(&sandbox_id)
                .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
            let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
            let current = sandbox
                .workspaces
                .get(&workspace_id)
                .cloned()
                .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
            // The sandbox record is needed by the host cleanup, which runs outside
            // the lock, so it is captured here rather than read again later.
            Ok((sandbox_id, workspace_id, current, sandbox.metadata.clone()))
        })?;

    let mut journal = crate::operation::Journal::begin(
        state_dir,
        "workspace.stop",
        format!("{}/{}", sandbox_id, workspace_id),
    )?;
    // Take the inventory before the runtime is signalled: it is the list of what
    // this workspace owned, and the certificate re-checks it afterwards so a stop
    // proves the resources are gone rather than only that the calls returned.
    let inventory = crate::workspace::ResourceInventory::collect(&sandbox_id, &current);
    journal.phase("stop_runtime")?;
    // Record the in-flight transition so an observer sees that teardown is
    // underway, and so a crash mid-stop leaves evidence instead of a workspace
    // that still claims to be running.
    mark_workspace_stopping(state_dir, &sandbox_id, &workspace_id)?;
    let stop_runtime = crate::perf::Timer::new("workspace.stop.runtime");
    if let Some(pid) = current.runtime_pid {
        if let Err(error) = session::stop_session(pid, current.runtime_starttime_ticks) {
            drop(stop_runtime);
            let _ = journal.fail(format!("{error:#}"));
            return Err(error);
        }
    }
    drop(stop_runtime);

    journal.phase("cleanup_resources")?;
    let cleanup_phase = crate::perf::Timer::new("workspace.stop.cleanup");
    // The host cleanup runs before the registry lock is taken. It unmounts
    // storage, tears down the network, and removes cgroups: none of that reads or
    // writes the registry, and on a host where each call costs milliseconds it is
    // the dominant part of a stop. Holding the registry lock across it would
    // stall every other lifecycle request for its duration.
    let network = match cleanup::run_workspace_stop_cleanup(
        cleanup::WorkspaceStopCleanup {
            sandbox: sandbox_metadata,
            workspace: current.clone(),
        },
        false,
        false,
    ) {
        Ok(network) => network,
        Err(error) => {
            drop(cleanup_phase);
            let _ = journal.fail(format!("{error:#}"));
            return Err(error);
        }
    };
    let result = with_registry_mut(state_dir, |registry| {
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let latest = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        if latest.runtime_pid != current.runtime_pid
            || latest.runtime_starttime_ticks != current.runtime_starttime_ticks
        {
            bail!(
                "workspace '{}' changed while it was stopping; refusing stale metadata commit",
                workspace_id
            );
        }
        let certificate = commit_workspace_stopped(sandbox, &workspace_id, network.as_ref())?
            .with_inventory(inventory);
        let result = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        Ok((result, certificate))
    });
    drop(cleanup_phase);
    match result {
        Ok((metadata, certificate)) => {
            journal.succeed()?;
            Ok((metadata, certificate))
        }
        Err(error) => {
            let _ = journal.fail(format!("{error:#}"));
            Err(error)
        }
    }
}

/// Record that teardown of a workspace runtime has begun.
pub(crate) fn mark_workspace_stopping(
    state_dir: &std::path::Path,
    sandbox_id: &str,
    workspace_id: &str,
) -> Result<()> {
    with_registry_mut(state_dir, |registry| {
        let workspace = registry
            .sandboxes
            .get_mut(sandbox_id)
            .and_then(|sandbox| sandbox.workspaces.get_mut(workspace_id))
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        workspace.status = WorkspaceStatus::Stopping;
        persist_workspace_metadata(workspace)
    })
}
