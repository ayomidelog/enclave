//! Launching one workspace runtime, in the order the host needs.
//!
//! Storage, the session, its cgroup limits, auth, and networking each depend on
//! the one before, and each is timed separately because all of them are on the
//! user-visible start path. A failure releases what the launch already created
//! rather than leaving it for a later reconcile.

use super::super::*;

pub fn launch_workspace_runtime(
    state_dir: &std::path::Path,
    sandbox_snapshot: &SandboxMetadata,
    workspace_snapshot: &WorkspaceMetadata,
    apparmor_profile: Option<&str>,
    selinux_label: Option<&str>,
    reserved_ip: &str,
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
    // The address was reserved under the registry lock before the launch began,
    // so a concurrent start cannot hand the same one to another workspace.
    let assigned_ip = network::setup_reserved_workspace_network(
        session_info.pid,
        reserved_ip,
        &workspace_rootfs,
        &workspace_snapshot.id,
    );
    let assigned_ip = match assigned_ip {
        Ok(ip) => ip,
        Err(err) => {
            drop(network);
            // Nothing owns the workspace yet, so the launch's own resources are
            // released here rather than left for a later reconcile.
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
