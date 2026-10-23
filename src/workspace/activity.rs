//! Persisted activity timestamps and the rules that turn them into idle state.
//!
//! The registry is the durable record, so an activity timestamp has to live on
//! the workspace record rather than only in daemon memory. The daemon still
//! owns the live half of the question: a timestamp can say when a command began,
//! but only a lease held for the command's lifetime can say it is still running.

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};

use crate::registry::with_registry_mut;
use crate::workspace::types::{WorkspaceMetadata, WorkspaceStatus};

/// What kind of request touched a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    /// A command, copy, enter, start, or published-port operation.
    Request,
    /// A foreground or detached job made progress.
    Process,
}

/// The clock used for every activity timestamp.
pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Parse a timestamp written by [`now`].
pub fn parse_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

/// The most recent activity on a workspace, preferring process progress over a
/// request when both exist.
pub fn last_activity(workspace: &WorkspaceMetadata) -> Option<DateTime<Utc>> {
    [
        workspace.last_request_at.as_deref(),
        workspace.last_process_activity_at.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter_map(parse_timestamp)
    .max()
}

/// Seconds since the workspace's most recent activity, when one is recorded.
pub fn idle_for_secs(workspace: &WorkspaceMetadata, now: DateTime<Utc>) -> Option<i64> {
    last_activity(workspace).map(|last| (now - last).num_seconds().max(0))
}

/// Update one workspace's activity timestamp, and persist both copies of its
/// record. A missing workspace is an ordinary race with destroy, so it is not
/// turned into an error here.
pub fn touch(
    state_dir: &Path,
    sandbox_id: &str,
    workspace_id: &str,
    kind: ActivityKind,
) -> Result<()> {
    let stamp = now();
    let updated = with_registry_mut(state_dir, |registry| {
        let Some(sandbox) = registry.sandboxes.get_mut(sandbox_id) else {
            return Ok(false);
        };
        let Some(workspace) = sandbox.workspaces.get_mut(workspace_id) else {
            return Ok(false);
        };
        match kind {
            ActivityKind::Request => workspace.last_request_at = Some(stamp.clone()),
            ActivityKind::Process => {
                workspace.last_request_at = Some(stamp.clone());
                workspace.last_process_activity_at = Some(stamp.clone());
            }
        }
        crate::workspace::control::persist_workspace_metadata(workspace)?;
        Ok(true)
    })?;
    if !updated {
        return Ok(());
    }
    Ok(())
}

/// Update a workspace timestamp without writing the per-workspace metadata file.
///
/// Activity is high-frequency bookkeeping: the registry write is the durable
/// record every sweep reads, while the per-workspace copy is a human-readable
/// projection that the next lifecycle mutation rewrites. Avoiding an fsync on
/// every command is what keeps a lease touch from becoming a lifecycle-cost-sized
/// operation.
pub fn touch_registry_only(
    state_dir: &Path,
    sandbox_id: &str,
    workspace_id: &str,
    kind: ActivityKind,
) -> Result<()> {
    let stamp = now();
    with_registry_mut(state_dir, |registry| {
        let Some(workspace) = registry
            .sandboxes
            .get_mut(sandbox_id)
            .and_then(|sandbox| sandbox.workspaces.get_mut(workspace_id))
        else {
            return Ok(());
        };
        match kind {
            ActivityKind::Request => workspace.last_request_at = Some(stamp),
            ActivityKind::Process => {
                workspace.last_request_at = Some(stamp.clone());
                workspace.last_process_activity_at = Some(stamp);
            }
        }
        Ok(())
    })
}

/// Whether a workspace is eligible for an idle stop right now.
///
/// A pinned workspace, a transitional lifecycle state, a foreground command, or
/// an activity timestamp newer than the threshold all make it ineligible.
pub fn is_idle(
    workspace: &WorkspaceMetadata,
    idle_after_secs: u64,
    now: DateTime<Utc>,
    active_commands: usize,
) -> bool {
    if workspace.status != WorkspaceStatus::Running
        || workspace.keep_running
        || idle_after_secs == 0
        || active_commands > 0
    {
        return false;
    }
    idle_for_secs(workspace, now).is_some_and(|idle| idle >= idle_after_secs as i64)
}

/// Record a stop time on the workspace's durable record.
pub fn mark_stopped(workspace: &mut WorkspaceMetadata) {
    workspace.last_stopped_at = Some(now());
}

/// The effective idle threshold for one workspace.
pub fn idle_threshold(workspace: &WorkspaceMetadata, daemon_default: Option<u64>) -> Option<u64> {
    workspace.idle_stop_after_secs.or(daemon_default)
}

/// The effective destroy-after threshold for one workspace.
pub fn destroy_threshold(
    workspace: &WorkspaceMetadata,
    daemon_default: Option<u64>,
) -> Option<u64> {
    workspace.destroy_after_secs.or(daemon_default)
}
