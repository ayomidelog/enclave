//! Running commands in a workspace and moving data in and out of it.

use super::*;

pub(in crate::daemon::dispatch) fn dispatch_workspace_exec(
    params: &Value,
    config: &DaemonConfig,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let cwd = params.get("cwd").and_then(Value::as_str).unwrap_or("/home");
    let command = parse_string_array(params, "command")?;
    // Scrubbing is on unless the caller turns it off, so a caller that says
    // nothing gets the safe behaviour.
    let scrub = params.get("scrub").and_then(Value::as_bool).unwrap_or(true);

    let result = workspace::exec_workspace_command_with_options(
        &config.state_dir,
        sandbox,
        workspace_selector,
        cwd,
        &command,
        workspace::WorkspaceExecOptions { scrub },
    )?;
    Ok(serde_json::to_value(result)?)
}

/// Stop the command a workspace is running right now.
///
/// Resolves the workspace to its host path, then signals the process group the
/// helper published. This is the only way to stop a command the daemon started:
/// the command lives in the helper's pid namespace, so its own pid means nothing
/// on the host, but its process group is visible from both.
pub(in crate::daemon::dispatch) fn dispatch_workspace_exec_cancel(
    params: &Value,
    config: &DaemonConfig,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let workspace_path =
        crate::workspace::resolve_workspace_path(&config.state_dir, sandbox, workspace_selector)?;
    let outcome = crate::workspace::cancel_workspace_command(&workspace_path)?;
    Ok(serde_json::json!({
        "cancelled": outcome.cancelled,
        "already_finished": outcome.already_finished,
    }))
}

pub(in crate::daemon::dispatch) fn dispatch_workspace_cp(
    params: &Value,
    config: &DaemonConfig,
    client_stream: Option<&UnixStream>,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let src = require_param_str(params, &["src"])?;
    let dst = require_param_str(params, &["dst"])?;
    let direction = require_param_str(params, &["direction"])?;
    let gzip = params.get("gzip").and_then(Value::as_bool).unwrap_or(false);
    let result = workspace::copy_workspace_path_with_connection(
        &config.state_dir,
        sandbox,
        workspace,
        src,
        dst,
        direction,
        workspace::CopyOptions {
            gzip,
            client_stream,
        },
    )?;
    Ok(serde_json::to_value(result)?)
}

pub(in crate::daemon::dispatch) fn dispatch_workspace_logs(
    params: &Value,
    config: &DaemonConfig,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let tail = params
        .get("tail")
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    let offset = params.get("offset").and_then(Value::as_u64);
    let stream_id = params.get("stream_id").and_then(Value::as_str);
    let result = workspace::workspace_logs(
        &config.state_dir,
        sandbox,
        workspace_selector,
        tail,
        offset,
        stream_id,
    )?;
    Ok(serde_json::to_value(result)?)
}
