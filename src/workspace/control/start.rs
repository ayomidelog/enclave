use super::*;

pub fn start_workspace(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<WorkspaceMetadata> {
    start_workspace_with_security(state_dir, sandbox_selector, workspace_selector, None, None)
}

pub fn start_workspace_with_security(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    apparmor_profile: Option<&str>,
    selinux_label: Option<&str>,
) -> Result<WorkspaceMetadata> {
    let (sandbox_id, workspace_id, sandbox_snapshot, mut workspace_snapshot, used_ips) =
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
            // A competing lifecycle operation is already in flight; refuse
            // rather than launch a second runtime for the same workspace.
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
                collect_all_used_ip_octets(registry),
            ))
        })?;

    if workspace_snapshot.status == WorkspaceStatus::Running {
        if let Some((pid, starttime)) = workspace_snapshot
            .runtime_pid
            .zip(workspace_snapshot.runtime_starttime_ticks)
        {
            if session::process_matches(pid, Some(starttime)) {
                let (mount_ns, pid_ns) = session::read_namespace_refs(pid)?;
                return with_registry_mut(state_dir, |registry| {
                    let sandbox = registry
                        .sandboxes
                        .get_mut(&sandbox_id)
                        .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
                    let workspace = sandbox
                        .workspaces
                        .get_mut(&workspace_id)
                        .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
                    if workspace.runtime_pid != Some(pid)
                        || workspace.runtime_starttime_ticks != Some(starttime)
                    {
                        bail!("workspace runtime identity changed while refreshing namespace refs");
                    }
                    normalize_namespace_ref_paths(workspace);
                    session::write_namespace_ref_values(workspace, &mount_ns, &pid_ns)?;
                    Ok(workspace.clone())
                });
            }
        }
        workspace_snapshot.status = WorkspaceStatus::Stopped;
        workspace_snapshot.runtime_pid = None;
        workspace_snapshot.runtime_starttime_ticks = None;
        workspace_snapshot.assigned_ip = None;
    }

    let mut journal = crate::operation::Journal::begin(
        state_dir,
        "workspace.start",
        format!("{}/{}", sandbox_id, workspace_id),
    )?;
    journal.phase("launch_runtime")?;
    // Record the in-flight transition durably so a crash during launch is
    // visible to the next daemon start instead of looking like a stopped
    // workspace that never started.
    mark_workspace_starting(state_dir, &sandbox_id, &workspace_id)?;
    let started = match launch_workspace_runtime(
        state_dir,
        &sandbox_snapshot,
        &workspace_snapshot,
        apparmor_profile,
        selinux_label,
        NetworkStartPlan::AllocateFromUsedIps(used_ips),
    ) {
        Ok(started) => started,
        Err(error) => {
            let _ = mark_workspace_start_failed(state_dir, &sandbox_id, &workspace_id);
            let _ = journal.fail(format!("{error:#}"));
            return Err(error);
        }
    };

    journal.phase("commit_runtime_metadata")?;
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

/// Record that a workspace runtime launch has begun.
fn mark_workspace_starting(
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
        workspace.status = WorkspaceStatus::Starting;
        persist_workspace_metadata(workspace)
    })
}

/// Roll a failed launch back to `Stopped`, tearing down anything the partial
/// launch left behind (workspace storage mounts, cgroups, network, `/tmp`).
fn mark_workspace_start_failed(
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

pub(crate) fn launch_workspace_runtime(
    state_dir: &std::path::Path,
    sandbox_snapshot: &SandboxMetadata,
    workspace_snapshot: &WorkspaceMetadata,
    apparmor_profile: Option<&str>,
    selinux_label: Option<&str>,
    network_plan: NetworkStartPlan,
) -> Result<WorkspaceRuntimeStart> {
    // Each phase below is on the user-visible startup critical path. Timing
    // them separately is what makes a slow start attributable to storage,
    // namespace launch, cgroups, auth, or networking instead of "startup".
    let storage = crate::perf::Timer::new("workspace.start.storage");
    crate::workspace::ensure_workspace_storage_ready(workspace_snapshot)?;
    crate::workspace::verify_workspace_source(workspace_snapshot)?;
    drop(storage);

    let launch = crate::perf::Timer::new("workspace.start.session");
    let session_info = session::start_session(workspace_snapshot, apparmor_profile, selinux_label)?;
    drop(launch);

    let limits = crate::perf::Timer::new("workspace.start.cgroup");
    if let Err(err) = runtime_limits::apply_workspace_runtime_constraints(
        sandbox_snapshot,
        workspace_snapshot,
        session_info.pid,
    ) {
        drop(limits);
        if let Err(stop_err) =
            session::stop_session(session_info.pid, Some(session_info.starttime_ticks))
        {
            tracing::warn!(
                "failed to stop workspace session {} after cgroup setup failure: {stop_err:#}",
                session_info.pid
            );
        }
        if let Err(cleanup_err) = cleanup::remove_workspace_cgroups(
            sandbox_snapshot,
            &workspace_snapshot.id,
            Some(session_info.pid),
        ) {
            tracing::warn!("failed to clean cgroup after cgroup setup failure: {cleanup_err:#}");
        }
        return Err(err).context("failed to apply workspace cgroup limits");
    }
    drop(limits);

    let auth = crate::perf::Timer::new("workspace.start.auth");
    let workspace_rootfs_path = format!("/proc/{}/root", session_info.pid);
    let auth_manager = crate::auth::AuthManager::new(state_dir.to_path_buf());
    if let Err(err) = auth_manager.sync_workspace_auth(
        &workspace_rootfs_path,
        &workspace_snapshot.auth_providers,
        &workspace_snapshot.env_tokens,
    ) {
        drop(auth);
        if let Err(cleanup_err) = cleanup::remove_workspace_cgroups(
            sandbox_snapshot,
            &workspace_snapshot.id,
            Some(session_info.pid),
        ) {
            tracing::warn!("failed to clean cgroup after auth sync failure: {cleanup_err:#}");
        }
        if let Err(stop_err) =
            session::stop_session(session_info.pid, Some(session_info.starttime_ticks))
        {
            tracing::warn!(
                "failed to stop workspace session {} after auth sync failure: {stop_err:#}",
                session_info.pid
            );
        }
        return Err(err)
            .context("failed to sync workspace auth; attempted to stop workspace session");
    }
    drop(auth);

    let network = crate::perf::Timer::new("workspace.start.network");
    let workspace_rootfs = PathBuf::from(format!("/proc/{}/root", session_info.pid));
    let assigned_ip = match network_plan {
        NetworkStartPlan::AllocateFromUsedIps(used_ips) => {
            match network::setup_workspace_network(
                session_info.pid,
                &used_ips,
                &workspace_rootfs,
                &workspace_snapshot.id,
            ) {
                Ok(ip) => ip,
                Err(err) => {
                    drop(network);
                    if let Err(cleanup_err) = cleanup::remove_workspace_cgroups(
                        sandbox_snapshot,
                        &workspace_snapshot.id,
                        Some(session_info.pid),
                    ) {
                        tracing::warn!(
                            "failed to clean cgroup after network setup failure: {cleanup_err:#}"
                        );
                    }
                    if let Err(stop_err) =
                        session::stop_session(session_info.pid, Some(session_info.starttime_ticks))
                    {
                        tracing::warn!(
                            "failed to stop workspace session {} after network setup failure: {stop_err:#}",
                            session_info.pid
                        );
                    }
                    return Err(err).context(
                        "failed to attach or validate workspace networking; aborted workspace startup",
                    );
                }
            }
        }
    };
    drop(network);

    Ok(WorkspaceRuntimeStart {
        pid: session_info.pid,
        starttime_ticks: session_info.starttime_ticks,
        mount_ns: session_info.mount_ns,
        pid_ns: session_info.pid_ns,
        assigned_ip,
    })
}
