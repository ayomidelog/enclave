use std::path::Path;
use std::process::{Child, Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};

use crate::registry::with_registry;
use crate::sandbox::resolve_sandbox_id;
use crate::workspace::sanitize_workspace_cwd;

use super::control::resolve_workspace_id;
use super::logs;
use super::session;
use super::types::{WorkspaceExecResult, WorkspaceMetadata, WorkspaceStatus};

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

    let effective_cwd = sanitize_workspace_cwd(cwd);
    let output = crate::workspace::with_workspace_storage_mounted(&workspace, || {
        session::execute_persistent_command(&workspace, &effective_cwd, command)
    })?;

    let exit_code = output.exit_code;
    let stdout = output.stdout;
    let stderr = output.stderr;
    let runtime_pid = workspace.runtime_pid.ok_or_else(|| {
        anyhow!(
            "workspace '{}' has no runtime pid after command execution",
            workspace.id
        )
    })?;
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

pub(crate) fn spawn_workspace_command(
    workspace: &WorkspaceMetadata,
    cwd: &str,
    command: &[String],
    stdin: Stdio,
    stdout: Stdio,
    stderr: Stdio,
) -> Result<Child> {
    if command.is_empty() {
        bail!("workspace command helper requires a command");
    }
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
    let current_exe = crate::workspace::session::resolve_session_helper_source();
    let namespace_fds =
        crate::workspace::session::duplicate_for_child(runtime_pid, runtime_starttime_ticks)?;
    let fds = crate::workspace::session::raw_fds(&namespace_fds);
    crate::perf::record_process_spawn();
    Command::new(&current_exe)
        .args(runtime_exec_command_args_with_fds(
            runtime_pid,
            runtime_starttime_ticks,
            workspace.sandbox_id.as_str(),
            workspace.id.as_str(),
            cwd,
            command,
            fds,
        ))
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .context("failed to execute workspace command via internal helper")
}

pub(crate) fn spawn_workspace_file_receiver(
    workspace: &WorkspaceMetadata,
    target: &str,
    stdin: Stdio,
    stderr: Stdio,
) -> Result<Child> {
    let runtime_pid = workspace.runtime_pid.ok_or_else(|| {
        anyhow!(
            "workspace '{}' has no runtime pid; restart workspace",
            workspace.id
        )
    })?;
    let runtime_starttime_ticks = workspace.runtime_starttime_ticks.ok_or_else(|| {
        anyhow!(
            "workspace '{}' has no runtime starttime; restart workspace",
            workspace.id
        )
    })?;
    if !session::process_matches(runtime_pid, Some(runtime_starttime_ticks)) {
        bail!("workspace '{}' runtime pid is not alive", workspace.id);
    }
    let current_exe = crate::workspace::session::resolve_session_helper_source();
    let namespace_fds =
        crate::workspace::session::duplicate_for_child(runtime_pid, runtime_starttime_ticks)?;
    let fds = crate::workspace::session::raw_fds(&namespace_fds);
    crate::perf::record_process_spawn();
    let mut args = vec![
        "internal".to_string(),
        "workspace-file-receive".to_string(),
        "--runtime-pid".to_string(),
        runtime_pid.to_string(),
        "--runtime-starttime-ticks".to_string(),
        runtime_starttime_ticks.to_string(),
        "--target".to_string(),
        target.to_string(),
    ];
    append_namespace_fd_args(&mut args, fds);
    Command::new(current_exe)
        .args(args)
        .stdin(stdin)
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .context("failed to execute workspace file receiver")
}

#[cfg(test)]
fn runtime_exec_command_args(
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    sandbox_id: &str,
    workspace_id: &str,
    effective_cwd: &str,
    command: &[String],
) -> Vec<String> {
    runtime_exec_command_args_base(
        runtime_pid,
        runtime_starttime_ticks,
        sandbox_id,
        workspace_id,
        effective_cwd,
        command,
        None,
    )
}

fn runtime_exec_command_args_with_fds(
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    sandbox_id: &str,
    workspace_id: &str,
    effective_cwd: &str,
    command: &[String],
    fds: [std::os::fd::RawFd; 6],
) -> Vec<String> {
    runtime_exec_command_args_base(
        runtime_pid,
        runtime_starttime_ticks,
        sandbox_id,
        workspace_id,
        effective_cwd,
        command,
        Some(fds),
    )
}

fn runtime_exec_command_args_base(
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    sandbox_id: &str,
    workspace_id: &str,
    effective_cwd: &str,
    command: &[String],
    fds: Option<[std::os::fd::RawFd; 6]>,
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
    if let Some(fds) = fds {
        append_namespace_fd_args(&mut args, fds);
    }
    args.push("--".to_string());
    args.extend(command.iter().cloned());
    args
}

fn append_namespace_fd_args(args: &mut Vec<String>, fds: [std::os::fd::RawFd; 6]) {
    for (name, fd) in [
        ("--root-fd", fds[0]),
        ("--user-ns-fd", fds[1]),
        ("--mount-ns-fd", fds[2]),
        ("--pid-ns-fd", fds[3]),
        ("--net-ns-fd", fds[4]),
        ("--uts-ns-fd", fds[5]),
    ] {
        args.push(name.to_string());
        args.push(fd.to_string());
    }
}

#[cfg(test)]
#[path = "../../tests/src/workspace/exec.rs"]
mod tests;
