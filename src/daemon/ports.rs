//! Republishing the ports a running workspace declared.
//!
//! The port publisher lives in the daemon, so it starts empty on every daemon
//! start. A workspace that is already running declared its ports to the previous
//! daemon, so they are re-established here rather than waiting for the next start.
//!
//! A paused sandbox is the exception, and it is why the sandbox's own state is
//! read here rather than only the workspace's. Pausing a sandbox freezes its
//! workspaces and withdraws their host listeners, so an operator can bind the
//! ports a paused sandbox declared. The workspaces keep the `running` status
//! through the pause, so re-establishing the listeners for every running
//! workspace would hand the paused sandbox's ports back on the next daemon start
//! and take them from whoever bound them in the meantime.

use std::path::Path;

use anyhow::Result;

use crate::registry::{with_registry, Registry};

use crate::sandbox::SandboxStatus;

/// The sandboxes whose workspaces may hold their published ports right now.
///
/// Only a running sandbox's workspaces can answer on a port, so only those are
/// republished. A paused sandbox withdrew its listeners deliberately and a
/// stopped one never had any to withdraw here.
fn sandboxes_holding_ports(registry: &Registry) -> Vec<String> {
    registry
        .sandboxes
        .values()
        .filter(|sandbox| sandbox.metadata.status == SandboxStatus::Running)
        .map(|sandbox| sandbox.metadata.id.clone())
        .collect()
}

pub(super) fn reconcile_published_ports(
    state_dir: &Path,
    port_publisher: &crate::network::publish::PortPublisher,
) -> Result<()> {
    let running_sandboxes =
        with_registry(state_dir, |registry| Ok(sandboxes_holding_ports(registry)))?;
    let workspaces = crate::workspace::list_workspaces(state_dir, None)?;
    for workspace in workspaces {
        if !workspace.status.is_running() || workspace.published_ports.is_empty() {
            continue;
        }
        // The workspace's own status does not change when its sandbox is paused,
        // so the sandbox is what decides whether the listeners belong here.
        if !running_sandboxes.contains(&workspace.sandbox_id) {
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

#[cfg(test)]
#[path = "../../tests/src/daemon/ports.rs"]
mod tests;
