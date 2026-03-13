use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::registry::with_registry;
use crate::sandbox::resolve_sandbox_id;
use crate::workspace::sanitize_workspace_cwd;

use super::control::resolve_workspace_id;
use super::logs;
use super::session;
use super::types::{WorkspaceExecResult, WorkspaceStatus};

pub fn exec_workspace_command(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    cwd: &str,
    command: &[String],
) -> Result<WorkspaceExecResult> {
    if command.is_empty() {
        bail!("workspace exec requires a command");
    }

    let workspace = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get(&workspace_id)
            .ok_or_else(|| {
                anyhow!(
                    "workspace '{}' not found in sandbox '{}'",
                    workspace_id,
                    sandbox_id
                )
            })?
            .clone();
        Ok(workspace)
    })?;

    if workspace.status != WorkspaceStatus::Running {
        bail!(
            "workspace '{}' is stopped; start workspace first",
            workspace.id
        );
    }

    let runtime_pid = workspace.runtime_pid.ok_or_else(|| {
        anyhow!(
            "workspace '{}' has no runtime pid; restart workspace",
            workspace.id
        )
    })?;
    if !session::process_matches(runtime_pid, workspace.runtime_starttime_ticks) {
        bail!(
            "workspace '{}' runtime pid {} is not alive; restart workspace",
            workspace.id,
            runtime_pid
        );
    }
    let runtime_starttime_ticks = workspace.runtime_starttime_ticks.ok_or_else(|| {
        anyhow!(
            "workspace '{}' has no runtime starttime; restart workspace",
            workspace.id
        )
    })?;

    let effective_cwd = sanitize_workspace_cwd(cwd);
    let current_exe = std::env::current_exe().context("failed to resolve current executable")?;
    let mut cmd = Command::new(&current_exe);
    cmd.args(runtime_exec_command_args(
        runtime_pid,
        runtime_starttime_ticks,
        workspace.sandbox_id.as_str(),
        workspace.id.as_str(),
        &effective_cwd,
        command,
    ));

    let output = cmd
        .output()
        .context("failed to execute workspace command via internal helper")?;

    let exit_code = output
        .status
        .code()
        .unwrap_or(if output.status.success() { 0 } else { 1 });
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let (mount_ns, pid_ns) = session::read_namespace_refs(runtime_pid)
        .unwrap_or_else(|_| ("unknown".to_string(), "unknown".to_string()));
    if let Err(err) = logs::append_workspace_command_log(
        &workspace,
        &effective_cwd,
        command,
        exit_code,
        &stdout,
        &stderr,
    ) {
        tracing::warn!(
            "failed to append command log for workspace {}: {err:#}",
            workspace.id
        );
    }

    Ok(WorkspaceExecResult {
        exit_code,
        stdout,
        stderr,
        mount_ns,
        pid_ns,
    })
}

fn runtime_exec_command_args(
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    sandbox_id: &str,
    workspace_id: &str,
    effective_cwd: &str,
    command: &[String],
) -> Vec<String> {
    let mut args = vec![
        "internal".to_string(),
        "workspace-command".to_string(),
        "--runtime-pid".to_string(),
        runtime_pid.to_string(),
        "--runtime-starttime-ticks".to_string(),
        runtime_starttime_ticks.to_string(),
        "--cwd".to_string(),
        effective_cwd.to_string(),
        "--sandbox-id".to_string(),
        sandbox_id.to_string(),
        "--workspace-id".to_string(),
        workspace_id.to_string(),
    ];
    args.extend(command.iter().cloned());
    args
}

#[cfg(test)]
#[path = "../../tests/src/workspace/exec.rs"]
mod tests;
