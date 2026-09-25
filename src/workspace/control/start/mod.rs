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
pub(crate) use transition::{mark_workspace_start_failed, mark_workspace_starting};

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

    let journal_timer = crate::perf::Timer::new("workspace.start.journal");
    let mut journal = crate::operation::Journal::begin(
        state_dir,
        "workspace.start",
        format!("{}/{}", sandbox_id, workspace_id),
    )?;
    // The session mounts the workspace root overlay on top of the sandbox
    // rootfs, so a sandbox whose rootfs bind is missing would hand the workspace
    // an empty root. The registry already proved the sandbox is `running`, so
    // re-establishing the mount is the repair, and the emptiness check turns a
    // silent wrong-root start into a reported failure.
    journal.phase("verify_sandbox_rootfs")?;
    if let Err(error) = crate::sandbox::ensure_rootfs_ready_for_workspace(&sandbox_snapshot) {
        let _ = journal.fail(format!("{error:#}"));
        return Err(error);
    }
    drop(journal_timer);
    journal.phase("launch_runtime")?;
    // Record the in-flight transition durably so a crash during launch is
    // visible to the next daemon start instead of looking like a stopped
    // workspace that never started.
    // The address is reserved under the registry lock so two workspaces starting
    // at the same time cannot both take the first free one. Reserving it here
    // rather than committing it at the end is what makes the batch start path
    // safe: those workers read the registry before any of them has committed.
    let registry_timer = crate::perf::Timer::new("workspace.start.reserve");
    let reserved_ip = match mark_workspace_starting(state_dir, &sandbox_id, &workspace_id) {
        Ok(ip) => ip,
        Err(error) => {
            let _ = journal.fail(format!("{error:#}"));
            return Err(error);
        }
    };
    drop(registry_timer);
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
        // captured below cannot be trusted as the current one.
        if workspace.status != WorkspaceStatus::Starting {
            bail!(
                "workspace '{}' is {} while a start was in progress; refusing to commit runtime metadata",
                workspace.id,
                workspace.status.as_str()
            );
        }
        workspace.sandbox_rootfs_path = workspace_snapshot.sandbox_rootfs_path.clone();
        workspace.status = WorkspaceStatus::Running;
        workspace.runtime_pid = Some(started.pid);
        workspace.runtime_starttime_ticks = Some(started.starttime_ticks);
        workspace.assigned_ip = Some(started.assigned_ip.clone());
        normalize_namespace_ref_paths(workspace);
        session::write_namespace_ref_values(workspace, &started.mount_ns, &started.pid_ns)?;

        let metadata_path = PathBuf::from(&workspace.workspace_path).join("workspace.json");
        let metadata_raw = serde_json::to_string_pretty(workspace)?;
        crate::fsutil::write_file_atomic(&metadata_path, metadata_raw.as_bytes(), 0o600)
            .with_context(|| {
                format!(
                    "failed to write workspace metadata {}",
                    metadata_path.display()
                )
            })?;
        Ok(workspace.clone())
    });
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
