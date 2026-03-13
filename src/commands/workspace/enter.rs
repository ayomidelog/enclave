use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::cli::{WorkspaceEnterArgs, WorkspaceExecArgs};
use crate::workspace::sanitize_workspace_cwd;
use crate::workspace::WorkspaceRuntimeInfo;

use super::super::send_managed;
use super::WorkspaceCommandContext;

pub(super) fn run_workspace_enter(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceEnterArgs,
) -> Result<()> {
    tracing::info!(
        "entering workspace '{}' in sandbox '{}'...",
        args.workspace,
        args.sandbox
    );
    let response = send_managed(
        ctx.socket,
        "workspace.runtime",
        serde_json::json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
        }),
    )?;
    let runtime: WorkspaceRuntimeInfo = serde_json::from_value(response)?;

    let requested_shell = args
        .shell
        .as_deref()
        .map(str::to_string)
        .or_else(|| std::env::var("SHELL").ok())
        .unwrap_or_else(|| "/bin/sh".to_string());
    let shell = choose_shell_in_rootfs(&runtime.sandbox_rootfs_path, &requested_shell)?;
    run_internal_workspace_command(runtime, &args.cwd, &[shell, "-i".to_string()], false)
}

pub(super) fn run_workspace_exec_direct(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceExecArgs,
) -> Result<()> {
    if args.command.is_empty() {
        bail!("workspace exec requires at least one command argument");
    }
    let response = send_managed(
        ctx.socket,
        "workspace.runtime",
        serde_json::json!({
            "sandbox": args.sandbox_id,
            "workspace": args.workspace_id,
        }),
    )?;
    let runtime: WorkspaceRuntimeInfo = serde_json::from_value(response)?;
    run_internal_workspace_command(runtime, &args.cwd, &args.command, true)
}

fn run_internal_workspace_command(
    runtime: WorkspaceRuntimeInfo,
    cwd: &str,
    command: &[String],
    forward_non_zero: bool,
) -> Result<()> {
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let effective_cwd = sanitize_workspace_cwd(cwd);
    let status = Command::new(exe)
        .arg("internal")
        .arg("workspace-command")
        .arg("--runtime-pid")
        .arg(runtime.runtime_pid.to_string())
        .arg("--runtime-starttime-ticks")
        .arg(runtime.runtime_starttime_ticks.to_string())
        .arg("--cwd")
        .arg(&effective_cwd)
        .arg("--sandbox-id")
        .arg(&runtime.sandbox_id)
        .arg("--workspace-id")
        .arg(&runtime.workspace_id)
        .args(command)
        .status()
        .context("failed to start internal workspace command helper")?;
    let code = status
        .code()
        .unwrap_or(if status.success() { 0 } else { 1 });
    if forward_non_zero && code != 0 {
        std::process::exit(code);
    }
    if !forward_non_zero && code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

fn choose_shell_in_rootfs(rootfs: &str, requested_shell: &str) -> Result<String> {
    let requested_path = requested_shell.trim();
    if is_valid_shell_path(requested_path) && is_executable_in_rootfs(rootfs, requested_path) {
        return Ok(requested_path.to_string());
    }
    if is_executable_in_rootfs(rootfs, "/bin/bash") {
        return Ok("/bin/bash".to_string());
    }
    if is_executable_in_rootfs(rootfs, "/bin/sh") {
        return Ok("/bin/sh".to_string());
    }
    bail!(
        "no usable shell found in rootfs '{}'; checked requested '{}' plus /bin/bash and /bin/sh",
        rootfs,
        requested_path
    );
}

fn is_valid_shell_path(path: &str) -> bool {
    !path.is_empty()
        && path.starts_with('/')
        && !path.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn is_executable_in_rootfs(rootfs: &str, absolute_binary_path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if !absolute_binary_path.starts_with('/') {
        return false;
    }
    let full_path = Path::new(rootfs).join(absolute_binary_path.trim_start_matches('/'));
    match full_path.symlink_metadata() {
        Ok(meta) => meta.is_file() && (meta.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

#[cfg(test)]
#[path = "../../../tests/src/commands/workspace/enter.rs"]
mod tests;
