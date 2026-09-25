//! Launching a long-running workspace command without holding the request open.

use std::process::Stdio;
use std::thread;

use anyhow::Result;

use crate::workspace::types::WorkspaceMetadata;

/// Launch a long-running command without keeping the daemon request open.
/// The helper wrapper is attached to the workspace cgroup before it forks the
/// command, so descendants follow the workspace lifecycle.
pub(crate) fn spawn_workspace_command_detached(
    workspace: &WorkspaceMetadata,
    cwd: &str,
    command: &[String],
) -> Result<()> {
    let child = super::spawn_workspace_command(
        workspace,
        cwd,
        command,
        Stdio::null(),
        Stdio::null(),
        Stdio::null(),
    )?;
    thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}
