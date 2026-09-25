//! Bringing a workspace runtime up.
//!
//! A start is three things: the decision and the bookkeeping that surround it,
//! the launch itself, and the transitions the registry records around the
//! launch. Each is a module so the launch sequence reads as the sequence of host
//! steps it is, and the registry transitions are together in one place.

mod launch;
mod transition;

use super::*;

pub use launch::launch_workspace_runtime;
pub(crate) use transition::{
    mark_workspace_start_failed, mark_workspace_starting, persist_started_workspace_runtime,
};

pub fn start_workspace(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<WorkspaceMetadata> {
    start_workspace_with_security(state_dir, sandbox_selector, workspace_selector, None, None)
}

/// Resolve the start target and refuse it if it cannot be started.
///
/// The sandbox has to be running and neither side may be mid-operation: a
/// competing lifecycle request is already changing the same records, and
/// launching a second runtime for a workspace another operation is tearing down
/// is exactly what the transitional states exist to prevent.
fn resolve_start_target(
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
fn refresh_live_runtime(
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
fn release_dead_runtime(
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

pub fn start_workspace_with_security(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    apparmor_profile: Option<&str>,
    selinux_label: Option<&str>,
) -> Result<WorkspaceMetadata> {
    let resolve = crate::perf::Timer::new("workspace.start.resolve");
    let (sandbox_id, workspace_id, sandbox_snapshot, mut workspace_snapshot) =
        resolve_start_target(state_dir, sandbox_selector, workspace_selector)?;
    drop(resolve);

    // A record that already says `running` is either a live runtime whose
    // namespace references need refreshing, or a dead one whose resources are
    // now unowned. Either way the start is answered without launching.
    if workspace_snapshot.status == WorkspaceStatus::Running {
        if let Some(metadata) = refresh_live_runtime(state_dir, &sandbox_id, &workspace_snapshot)? {
            return Ok(metadata);
        }
        release_dead_runtime(
            state_dir,
            &sandbox_id,
            &workspace_id,
            &mut workspace_snapshot,
        )?;
    }

    // Both intent records are written here, before any host state changes: the
    // journal, which is the audit trail of the operation, and the registry's
    // `Starting` transition with the address reserved for it. They are written
    // together rather than one after the other because they are independent files
    // and the filesystem commits concurrent durable writes in one transaction: two
    // writes that each cost about 9 ms take about 9 ms together, measured on this
    // host. Running them in sequence was the whole of the phase, so this is about
    // 12 ms of every start.
    //
    // Neither write is a host side effect, so no ordering between them is load
    // bearing; what matters is that both are on disk before the launch, which the
    // join below establishes. The failure handling is the reason this is written out
    // rather than chained: a journal that fails to be written after the reservation
    // succeeded has to give the reservation back, or the workspace is left `Starting`
    // with no operation that will ever finish it.
    let intent_timer = crate::perf::Timer::new("workspace.start.intent");
    let journal_target = format!("{}/{}", sandbox_id, workspace_id);
    let (journal_result, reserve_result) = std::thread::scope(|scope| {
        let journal = scope.spawn(|| {
            crate::operation::Journal::begin(state_dir, "workspace.start", journal_target)
        });
        let reserve =
            scope.spawn(|| mark_workspace_starting(state_dir, &sandbox_id, &workspace_id));
        (
            journal
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            reserve
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
        )
    });
    drop(intent_timer);
    let (mut journal, reserved_ip) = match (journal_result, reserve_result) {
        (Ok(journal), Ok(reserved_ip)) => (journal, reserved_ip),
        (Ok(journal), Err(error)) => {
            let _ = journal.fail(format!("{error:#}"));
            return Err(error);
        }
        (Err(error), reserve) => {
            // The journal is what would have described this start, so a failure to
            // write it is answered by giving the reservation back rather than by
            // leaving a `Starting` record with nothing to finish it.
            if reserve.is_ok() {
                if let Err(rollback) =
                    mark_workspace_start_failed(state_dir, &sandbox_id, &workspace_id)
                {
                    tracing::warn!(
                        "failed to release the reservation for workspace '{}' after the journal could not be written: {rollback:#}",
                        workspace_id
                    );
                }
            }
            return Err(error);
        }
    };
    // The session mounts the workspace root overlay on top of the sandbox
    // rootfs, so a sandbox whose rootfs bind is missing would hand the workspace
    // an empty root. The registry already proved the sandbox is `running`, so
    // re-establishing the mount is the repair, and the emptiness check turns a
    // silent wrong-root start into a reported failure.
    journal.phase("verify_sandbox_rootfs")?;
    if let Err(error) = crate::sandbox::ensure_rootfs_ready_for_workspace(&sandbox_snapshot) {
        // The reservation is already recorded by this point, so a failed pre-flight
        // has to give it back as well as close the journal. Leaving it would keep the
        // workspace `Starting` with the address held and nothing that will finish it.
        let _ = mark_workspace_start_failed(state_dir, &sandbox_id, &workspace_id);
        let _ = journal.fail(format!("{error:#}"));
        return Err(error);
    }
    journal.phase("launch_runtime")?;
    let started = match launch_workspace_runtime(
        state_dir,
        &sandbox_snapshot,
        &workspace_snapshot,
        apparmor_profile,
        selinux_label,
        &reserved_ip,
    ) {
        Ok(started) => started,
        Err(error) => {
            let _ = mark_workspace_start_failed(state_dir, &sandbox_id, &workspace_id);
            let _ = journal.fail(format!("{error:#}"));
            return Err(error);
        }
    };

    journal.phase("commit_runtime_metadata")?;
    let commit_timer = crate::perf::Timer::new("workspace.start.commit");
    // The workspace's own files go down first, outside the lock. See the note on
    // this function for why that is the order the precedence rule asks for and why
    // the lifecycle lease is what makes it safe.
    let prepared = match persist_started_workspace_runtime(&workspace_snapshot, &started) {
        Ok(prepared) => prepared,
        Err(error) => {
            drop(commit_timer);
            let _ = session::stop_session(started.pid, Some(started.starttime_ticks));
            let _ = cleanup::remove_workspace_cgroups(
                &sandbox_snapshot,
                &workspace_snapshot.id,
                Some(started.pid),
            );
            let _ = network::teardown_workspace_network(&started.assigned_ip, &workspace_id);
            let _ = crate::workspace::ensure_workspace_storage_unmounted(&workspace_snapshot);
            let _ = mark_workspace_start_failed(state_dir, &sandbox_id, &workspace_id);
            let _ = journal.fail(format!("{error:#}"));
            return Err(error).context("failed to record the launched workspace runtime");
        }
    };
    // Only the registry is written under the lock, and only after re-checking that
    // the workspace is still the one this launch was for. The registry half is
    // timed on its own because it is the part that holds the lock: a reader can
    // then see how long the lock was held rather than only how long the whole
    // commit took, and the difference between the two is what the file writes cost.
    let registry_commit = crate::perf::Timer::new("workspace.start.commit_registry");
    let commit = with_registry_mut(state_dir, |registry| {
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace = sandbox
            .workspaces
            .get_mut(&workspace_id)
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        // `mark_workspace_starting` recorded the transition before the runtime
        // was launched. Anything else means a competing operation touched the
        // workspace while the launch was in flight, so the runtime identity
        // captured above cannot be trusted as the current one.
        if workspace.status != WorkspaceStatus::Starting {
            bail!(
                "workspace '{}' is {} while a start was in progress; refusing to commit runtime metadata",
                workspace.id,
                workspace.status.as_str()
            );
        }
        workspace.sandbox_rootfs_path = prepared.sandbox_rootfs_path.clone();
        workspace.status = WorkspaceStatus::Running;
        workspace.runtime_pid = prepared.runtime_pid;
        workspace.runtime_starttime_ticks = prepared.runtime_starttime_ticks;
        workspace.assigned_ip = prepared.assigned_ip.clone();
        workspace.namespace_refs = prepared.namespace_refs.clone();
        Ok(workspace.clone())
    });
    drop(registry_commit);
    drop(commit_timer);

    match commit {
        Ok(metadata) => {
            journal.succeed()?;
            Ok(metadata)
        }
        Err(error) => {
            let _ = session::stop_session(started.pid, Some(started.starttime_ticks));
            let network_report =
                network::teardown_workspace_network(&started.assigned_ip, &workspace_id);
            let cgroup_cleanup = cleanup::remove_workspace_cgroups(
                &sandbox_snapshot,
                &workspace_snapshot.id,
                Some(started.pid),
            );
            let storage_cleanup =
                crate::workspace::ensure_workspace_storage_unmounted(&workspace_snapshot);
            let failure = if !network_report.is_complete()
                || cgroup_cleanup.is_err()
                || storage_cleanup.is_err()
            {
                error.context(format!(
                    "workspace startup rollback incomplete: network={:?}, cgroup={:?}, storage={:?}",
                    network_report.failures,
                    cgroup_cleanup.err(),
                    storage_cleanup.err()
                ))
            } else {
                error
            };
            let _ = mark_workspace_start_failed(state_dir, &sandbox_id, &workspace_id);
            let _ = journal.fail(format!("{failure:#}"));
            Err(failure)
        }
    }
}
