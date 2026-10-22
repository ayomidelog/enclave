use std::io::Write;
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
    let runtime = request_runtime_with_lease(ctx.socket, &args.sandbox, &args.workspace)?;
    let lease = runtime.command_lease.clone();

    let requested_shell = args
        .shell
        .as_deref()
        .map(str::to_string)
        .or_else(|| std::env::var("SHELL").ok())
        .unwrap_or_else(|| "/bin/sh".to_string());
    let shell = choose_shell_in_rootfs(&runtime.sandbox_rootfs_path, &requested_shell)?;
    let result = run_internal_workspace_command(runtime, &args.cwd, &[shell, "-i".to_string()]);
    release_command_lease(ctx, lease.as_deref());
    exit_with_command_status(result)
}

pub(super) fn run_workspace_exec_direct(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceExecArgs,
) -> Result<()> {
    if args.command.is_empty() {
        bail!("workspace exec requires at least one command argument");
    }

    if args.background {
        let response = send_managed(
            ctx.socket,
            "workspace.exec",
            serde_json::json!({
                "sandbox": args.sandbox_id,
                "workspace": args.workspace_id,
                "cwd": args.cwd,
                "command": args.command,
                "background": true,
                "keep_running": args.keep_running,
                "ttl_secs": args.ttl_secs,
            }),
        )?;
        println!(
            "job {}",
            response
                .get("job_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
        );
        return Ok(());
    }

    // The output of a command can contain a token the workspace holds, and the
    // output travels further than the workspace does. Scrubbing happens in the
    // daemon, which is the only side that holds the values, so this asks for the
    // command to be run there. `--no-scrub` keeps the direct streaming path,
    // which is what a caller who wants a terminal or a pipe needs.
    if args.no_scrub {
        tracing::warn!(
            "--no-scrub: the output of this command is printed exactly as it was written, including any injected token value"
        );
        let runtime = request_runtime_with_lease(ctx.socket, &args.sandbox_id, &args.workspace_id)?;
        let lease = runtime.command_lease.clone();
        let result = run_internal_workspace_command(runtime, &args.cwd, &args.command);
        release_command_lease(ctx, lease.as_deref());
        return exit_with_command_status(result);
    }

    let response = send_managed(
        ctx.socket,
        "workspace.exec",
        serde_json::json!({
            "sandbox": args.sandbox_id,
            "workspace": args.workspace_id,
            "cwd": args.cwd,
            "command": args.command,
            "scrub": true,
        }),
    )?;
    let result: crate::workspace::WorkspaceExecResult = serde_json::from_value(response)?;
    let mut stdout = std::io::stdout();
    stdout.write_all(result.stdout.as_bytes())?;
    stdout.flush()?;
    let mut stderr = std::io::stderr();
    stderr.write_all(result.stderr.as_bytes())?;
    stderr.flush()?;
    if result.exit_code != 0 {
        std::process::exit(result.exit_code);
    }
    Ok(())
}

/// Release the daemon-side lease after a direct CLI command finishes.
fn release_command_lease(ctx: &WorkspaceCommandContext<'_>, lease: Option<&str>) {
    if let Some(lease) = lease {
        let _ = send_managed(
            ctx.socket,
            "workspace.command.end",
            serde_json::json!({ "lease": lease }),
        );
    }
}

/// Propagate a command's exit status only after its lease has been released.
///
/// Calling `process::exit` inside the command runner would drop the lease object
/// without giving the explicit release request a chance to run. The daemon would
/// eventually reap the lease when this pid disappeared, but an idle sweep in that
/// window should still see the command as finished immediately.
fn exit_with_command_status(result: Result<()>) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Some(code) = command_exit_code(&error) {
                std::process::exit(code);
            }
            Err(error)
        }
    }
}

fn command_exit_code(error: &anyhow::Error) -> Option<i32> {
    error
        .downcast_ref::<CommandExitStatus>()
        .map(|status| status.0)
}

/// Ask the daemon for a runtime and take a lease owned by this CLI process.
///
/// The daemon cannot own this lease: it outlives every CLI invocation, so a
/// lease keyed by the daemon pid would never expire if the CLI crashed. The
/// command process's pid and start time are an identity the daemon can verify
/// on every sweep and that disappears with the command.
fn request_runtime_with_lease(
    socket: &Path,
    sandbox: &str,
    workspace: &str,
) -> Result<WorkspaceRuntimeInfo> {
    let owner_pid = std::process::id();
    let owner_starttime_ticks = process_starttime_ticks(owner_pid)?;
    let response = send_managed(
        socket,
        "workspace.command.begin",
        serde_json::json!({
            "sandbox": sandbox,
            "workspace": workspace,
            "owner_pid": owner_pid,
            "owner_starttime_ticks": owner_starttime_ticks,
        }),
    )?;
    let lease = response
        .get("lease")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let response = send_managed(
        socket,
        "workspace.runtime",
        serde_json::json!({
            "sandbox": sandbox,
            "workspace": workspace,
        }),
    );
    let mut runtime: WorkspaceRuntimeInfo = match response {
        Ok(response) => serde_json::from_value(response)?,
        Err(error) => {
            if let Some(lease) = lease.as_deref() {
                release_command_lease(&WorkspaceCommandContext { socket }, Some(lease));
            }
            return Err(error);
        }
    };
    runtime.command_lease = lease;
    Ok(runtime)
}

/// The start time of a process read directly from `/proc`.
///
/// The CLI runs outside a workspace but uses the same identity format the
/// daemon's process checks use, so a recycled pid cannot keep a stale lease
/// alive.
fn process_starttime_ticks(pid: u32) -> Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .with_context(|| format!("failed to read /proc/{pid}/stat"))?;
    let rest = stat
        .rsplit_once(") ")
        .map(|(_, rest)| rest)
        .context("failed to parse process start time")?;
    rest.split_whitespace()
        .nth(19)
        .context("process stat has no start-time field")?
        .parse::<u64>()
        .context("failed to parse process start time")
}

/// Run a command in the workspace and exit with its status.
///
/// The status is always propagated: a one-shot command that fails has to fail the
/// CLI, and an interactive shell that the operator exits non-zero is the shell's
/// own answer about the session. There is no case where the caller wants this to
/// swallow the code, which is what the parameter this replaced was for.
fn run_internal_workspace_command(
    runtime: WorkspaceRuntimeInfo,
    cwd: &str,
    command: &[String],
) -> Result<()> {
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let effective_cwd = sanitize_workspace_cwd(cwd);
    let status = Command::new(exe)
        .args(internal_workspace_command_args(
            &runtime,
            &effective_cwd,
            command,
        ))
        .status()
        .context("failed to start internal workspace command helper")?;
    // A process killed by a signal has no code, and it did not succeed, so it is
    // reported as a plain failure. The caller exits after releasing its lease.
    match status.code().unwrap_or(1) {
        0 => Ok(()),
        code => Err(anyhow::Error::new(CommandExitStatus(code))),
    }
}

#[derive(Debug)]
struct CommandExitStatus(i32);

impl std::fmt::Display for CommandExitStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "workspace command exited with status {}", self.0)
    }
}

impl std::error::Error for CommandExitStatus {}

/// Arguments for the helper that runs a command inside a workspace runtime.
///
/// The helper attaches itself to the workspace cgroup before it forks, which is
/// what makes the workspace's declared CPU, memory, and process limits apply to
/// the command. Without `--cgroup-path` the command would run in whatever cgroup
/// the caller happens to be in.
fn internal_workspace_command_args(
    runtime: &WorkspaceRuntimeInfo,
    cwd: &str,
    command: &[String],
) -> Vec<String> {
    let mut args = vec![
        "internal".to_string(),
        "workspace-command".to_string(),
        "--runtime-pid".to_string(),
        runtime.runtime_pid.to_string(),
        "--runtime-starttime-ticks".to_string(),
        runtime.runtime_starttime_ticks.to_string(),
        "--cwd".to_string(),
        cwd.to_string(),
        "--sandbox-id".to_string(),
        runtime.sandbox_id.clone(),
        "--workspace-id".to_string(),
        runtime.workspace_id.clone(),
    ];
    if let Some(cgroup_path) = runtime.cgroup_path.as_deref() {
        args.push("--cgroup-path".to_string());
        args.push(cgroup_path.to_string());
    }
    args.extend(command.iter().cloned());
    args
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
