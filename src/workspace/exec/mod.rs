//! Running commands inside a workspace.
//!
//! A command is executed by a helper that enters the workspace's namespaces, so
//! the module is split by how the helper is driven: the daemon-side entry point
//! resolves the workspace, runs the command, and records it; the arguments module
//! builds the helper's argument lists; and the detached module launches a
//! long-running command without keeping the request open.

mod args;
mod detached;

use std::path::Path;
use std::process::{Child, Stdio};

use anyhow::{anyhow, bail, Context, Result};

use crate::registry::with_registry;
use crate::sandbox::resolve_sandbox_id;
use crate::workspace::sanitize_workspace_cwd;

use super::control::resolve_workspace_id;
use super::logs;
use super::session;
use super::types::{WorkspaceExecResult, WorkspaceMetadata};

#[cfg(test)]
pub(crate) use args::runtime_exec_command_args;
#[cfg(test)]
pub(crate) use args::workspace_file_receive_args;
pub(crate) use detached::spawn_workspace_command_detached;

/// What a caller can ask of an exec.
#[derive(Debug, Clone, Copy)]
pub struct WorkspaceExecOptions {
    /// Replace injected token values in the captured output.
    ///
    /// On by default: the workspace is entitled to its token, and the output of
    /// a command travels further than the workspace does.
    pub scrub: bool,
}

impl Default for WorkspaceExecOptions {
    fn default() -> Self {
        Self { scrub: true }
    }
}

pub fn exec_workspace_command(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    cwd: &str,
    command: &[String],
) -> Result<WorkspaceExecResult> {
    exec_workspace_command_with_options(
        state_dir,
        sandbox_selector,
        workspace_selector,
        cwd,
        command,
        WorkspaceExecOptions::default(),
    )
}

pub fn exec_workspace_command_with_options(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    cwd: &str,
    command: &[String],
    options: WorkspaceExecOptions,
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

    if !workspace.status.is_running() {
        bail!(
            "workspace '{}' is {}; start workspace first",
            workspace.id,
            workspace.status.as_str()
        );
    }

    let effective_cwd = sanitize_workspace_cwd(cwd);
    let output = crate::workspace::with_workspace_storage_mounted(&workspace, || {
        session::execute_persistent_command(&workspace, &effective_cwd, command)
    })?;

    let exit_code = output.exit_code;
    let mut stdout = output.stdout;
    let mut stderr = output.stderr;
    if options.scrub {
        scrub_injected_tokens(state_dir, &workspace, &mut stdout, &mut stderr)?;
    }
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

/// Replace the workspace's own token values in captured output.
///
/// The values come from the same namespace the start injected from, so a
/// workspace is scrubbed of the tokens it actually holds. Each token that was
/// scrubbed is also recorded, which is the event an operator asking "when was
/// this credential used" wants.
fn scrub_injected_tokens(
    state_dir: &Path,
    workspace: &WorkspaceMetadata,
    stdout: &mut String,
    stderr: &mut String,
) -> Result<()> {
    let manager = crate::auth::AuthManager::new(state_dir.to_path_buf());
    let tokens = manager.resolved_credentials(
        workspace.owner.as_deref(),
        &workspace.auth_providers,
        &workspace.env_tokens,
    );
    if tokens.is_empty() {
        return Ok(());
    }
    let secrets: Vec<String> = tokens.iter().map(|token| token.token.clone()).collect();
    *stdout = crate::auth::scrub_secrets(stdout, &secrets);
    *stderr = crate::auth::scrub_secrets(stderr, &secrets);
    let target = crate::auth::WorkspaceAuthTarget {
        sandbox_id: &workspace.sandbox_id,
        workspace_id: &workspace.id,
        owner: workspace.owner.as_deref(),
    };
    let providers: Vec<String> = tokens.iter().map(|token| token.provider.clone()).collect();
    if let Err(err) = manager.audit_inject(&target, &providers) {
        tracing::warn!(
            "failed to record the auth injection for workspace {}: {err:#}",
            workspace.id
        );
    }
    Ok(())
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
    if !workspace.status.is_running() {
        bail!(
            "workspace '{}' is {}; start workspace first",
            workspace.id,
            workspace.status.as_str()
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
    std::process::Command::new(&current_exe)
        .args(args::runtime_exec_command_args_with_fds(
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
    let args = args::workspace_file_receive_args(
        runtime_pid,
        runtime_starttime_ticks,
        target,
        super::existing_workspace_cgroup_path(&workspace.sandbox_id, &workspace.id).as_deref(),
        fds,
    );
    std::process::Command::new(current_exe)
        .args(args)
        .stdin(stdin)
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .context("failed to execute workspace file receiver")
}

#[cfg(test)]
#[path = "../../../tests/src/workspace/exec.rs"]
mod tests;
