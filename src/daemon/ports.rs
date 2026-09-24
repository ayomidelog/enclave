//! Republishing the ports a running workspace declared.
//!
//! The port publisher lives in the daemon, so it starts empty on every daemon
//! start. A workspace that is already running declared its ports to the previous
//! daemon, so they are re-established here rather than waiting for the next start.

use std::path::Path;

use anyhow::Result;

pub(super) fn reconcile_published_ports(
    state_dir: &Path,
    port_publisher: &crate::network::publish::PortPublisher,
) -> Result<()> {
    let workspaces = crate::workspace::list_workspaces(state_dir, None)?;
    for workspace in workspaces {
        if !workspace.status.is_running() || workspace.published_ports.is_empty() {
            continue;
        }

        let Some(runtime_pid) = workspace.runtime_pid else {
            tracing::warn!(
                "workspace {} is marked running without a runtime pid; skipping port republish",
                workspace.id
            );
            continue;
        };
        if !crate::workspace::session_process_matches(
            runtime_pid,
            workspace.runtime_starttime_ticks,
        ) {
            tracing::warn!(
                "workspace {} runtime pid {} is not alive; skipping port republish",
                workspace.id,
                runtime_pid
            );
            continue;
        }

        let Some(workspace_ip) = workspace.assigned_ip.as_deref() else {
            tracing::warn!(
                "workspace {} is running without an assigned IP; skipping port republish",
                workspace.id
            );
            continue;
        };

        port_publisher.reconcile_workspace_ports(
            &workspace.sandbox_id,
            &workspace.id,
            runtime_pid,
            workspace_ip,
            &workspace.published_ports,
        )?;
    }
    Ok(())
}
