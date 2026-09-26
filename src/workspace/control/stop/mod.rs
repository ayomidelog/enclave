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

    // Take the inventory before the runtime is signalled: it is the list of what
    // this workspace owned, and the certificate re-checks it afterwards so a stop
    // proves the resources are gone rather than only that the calls returned.
    let inventory = crate::workspace::ResourceInventory::collect(&sandbox_id, &current);
    // The two records a stop writes before it touches the runtime are its journal
    // and the registry's `Stopping` transition. They are independent files, so they
    // are written together rather than one after the other: the filesystem commits
    // concurrent durable writes in one transaction, and two writes that each cost
    // about 9 ms take about 9 ms together, measured on this host. Neither is a host
    // side effect, so no ordering between them is load bearing; what matters is that
    // both are on disk before the runtime is signalled, which the join establishes.
    //
    // `Stopping` is the record that teardown is underway, so a crash mid-stop leaves
    // evidence instead of a workspace that still claims to be running. Because the
    // journal write can now fail after it has landed, the failure handling is written
    // out rather than chained: each half is undone on its own below.
    let intent_timer = crate::perf::Timer::new("workspace.stop.intent");
    let journal_target = format!("{}/{}", sandbox_id, workspace_id);
    let (journal_result, stopping_result) = std::thread::scope(|scope| {
        let journal = scope.spawn(|| {
            crate::operation::Journal::begin(state_dir, "workspace.stop", journal_target)
        });
        let stopping =
            scope.spawn(|| mark_workspace_stopping(state_dir, &sandbox_id, &workspace_id));
        (
            journal
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            stopping
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
        )
    });
    drop(intent_timer);
    let mut journal = match (journal_result, stopping_result) {
        (Ok(journal), Ok(())) => journal,
        (Ok(journal), Err(error)) => {
            // The journal describes a stop that will not run, so it is closed rather
            // than left open: an open record reads as an operation still in flight,
            // and nothing is going to finish this one.
            let _ = journal.fail(format!("{error:#}"));
            return Err(error);
        }
        (Err(error), stopping) => {
            if stopping.is_ok() {
                rollback_workspace_stopping(state_dir, &sandbox_id, &workspace_id, &current.status);
            }
            return Err(error);
        }
    };
    // The phase note is the last thing the stop writes before it commits to tearing
    // the runtime down, so a failure here gives the `Stopping` transition back the
    // same way a journal failure does.
    if let Err(error) = journal.phase("stop_runtime") {
        let _ = journal.fail(format!("{error:#}"));
        rollback_workspace_stopping(state_dir, &sandbox_id, &workspace_id, &current.status);
        return Err(error);
    }
    let stop_runtime = crate::perf::Timer::new("workspace.stop.runtime");
    // A workspace left `Starting` by an interrupted launch has no pid in its
    // record, but its session may already be running. Signalling nothing and then
    // removing the runtime markers would leave that session with nothing on the
    // host naming it, so the session is found the same way the launch failure path
    // finds it: by the pid file the session wrote, or by its own command line.
    let in_flight = (current.status == WorkspaceStatus::Starting)
        .then(|| session::live_session_pid(&current))
        .flatten();
    let stop_target = current
        .runtime_pid
        .map(|pid| (pid, current.runtime_starttime_ticks))
        .or_else(|| {
            in_flight.map(|pid| {
                // The start time is read before the signal so the stop proves it is
                // the same process it ends rather than a reused pid.
                (pid, session::process_starttime_ticks(pid).ok())
            })
        });
    if let Some((pid, starttime_ticks)) = stop_target {
        if let Err(error) = session::stop_session(pid, starttime_ticks) {
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

/// Put back the status of a workspace whose stop could not begin.
///
/// A stop records `Stopping` before it can know whether the rest of its preamble
/// succeeded, so every failure between that write and the teardown has to put the
/// status back: a workspace left `Stopping` reads as an operation still in flight
/// and refuses the next start until a repair rolls it back.
///
/// The status is only put back while it is still `Stopping`, so a competing
/// transition that has already moved the workspace on is not clobbered. The rollback
/// is best effort: the caller is already returning the error that stopped the stop,
/// and a failure to write the rollback is reported rather than replacing it.
fn rollback_workspace_stopping(
    state_dir: &std::path::Path,
    sandbox_id: &str,
    workspace_id: &str,
    previous_status: &WorkspaceStatus,
) {
    let result = with_registry_mut(state_dir, |registry| {
        let workspace = registry
            .sandboxes
            .get_mut(sandbox_id)
            .and_then(|sandbox| sandbox.workspaces.get_mut(workspace_id))
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        if workspace.status != WorkspaceStatus::Stopping {
            return Ok(());
        }
        workspace.status = previous_status.clone();
        persist_workspace_metadata(workspace)
    });
    if let Err(error) = result {
        tracing::warn!(
            "failed to put workspace '{}' back to its pre-stop status after a stop could not begin: {error:#}",
            workspace_id
        );
    }
}
