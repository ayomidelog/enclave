//! Stopping the session a launch started when that launch does not finish.
//!
//! Readiness is reported by the session's own files, so a session that never
//! becomes ready is a failure the caller cannot act on: the runtime's pid is known
//! only to the pid file the session wrote, and the record the caller keeps has no
//! pid yet. Left alone, the session finishes its setup a moment later and becomes a
//! live runtime that nothing names. The failed start rewrites the record as
//! stopped, so not even repair can find it, and the process holds its namespaces,
//! its mounts, and its private `/tmp` for as long as the host runs.
//!
//! The session is found through [`live_session_pid`], which reads the pid file the
//! session wrote and falls back to the process's own command line. The pid file is
//! not always there to read: a stop that overlaps the launch removes the runtime
//! markers as it settles the record, and it can remove the pid file of the session
//! that is still starting.

use super::*;

/// Stop the session this launch started, if it is still alive.
///
/// A launch that fails before its session reports ready leaves a process nothing
/// describes, so this is the only chance to end it: after the caller rewrites the
/// record there is no pid, no cgroup, and no interface left to find it by. It is
/// best effort for the same reason the record is not: the caller is already
/// returning the failure that stopped the launch, and a session that cannot be
/// stopped is reported rather than replacing that failure.
pub(super) fn reap_launched_session(workspace: &WorkspaceMetadata) {
    let Some(pid) = live_session_pid(workspace) else {
        return;
    };
    if !namespace_marker_names(workspace, pid) {
        tracing::warn!(
            "not stopping pid {pid}: its pid namespace is not the one workspace '{}' recorded, so the number was reused",
            workspace.id
        );
        return;
    }
    let starttime = process_starttime_ticks(pid).ok();
    match stop_session(pid, starttime) {
        Ok(()) => tracing::warn!(
            "stopped session pid {pid}, which the failed launch of workspace '{}' left running",
            workspace.id
        ),
        Err(error) => tracing::warn!(
            "failed to stop session pid {pid} left by the failed launch of workspace '{}': {error:#}",
            workspace.id
        ),
    }
}

/// Whether the pid namespace of `pid` is the one the workspace's marker records.
///
/// An absent or empty marker is not evidence against the pid: the marker is written
/// from inside the session, so a session that is still starting has not written it,
/// and the pid file that named the process is the stronger of the two.
fn namespace_marker_names(workspace: &WorkspaceMetadata, pid: u32) -> bool {
    let (_, pid_ref_path) = namespace_ref_paths(workspace);
    let Ok(raw) = fs::read_to_string(&pid_ref_path) else {
        return true;
    };
    let recorded = raw.trim();
    if recorded.is_empty() || recorded == "unassigned" {
        return true;
    }
    fs::read_link(format!("/proc/{pid}/ns/pid"))
        .map(|link| link.to_string_lossy() == recorded)
        .unwrap_or(false)
}
