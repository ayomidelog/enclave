//! Running commands in a workspace and moving data in and out of it.

use super::*;

/// Register a direct CLI command so an idle sweep cannot stop the workspace
/// while the command is running.
pub(in crate::daemon::dispatch) fn dispatch_workspace_command_begin(
    params: &Value,
    config: &DaemonConfig,
    services: &crate::daemon::services::DaemonServices,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let owner_pid = params
        .get("owner_pid")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| anyhow::anyhow!("workspace.command.begin requires 'owner_pid'"))?;
    let owner_starttime = params
        .get("owner_starttime_ticks")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            anyhow::anyhow!("workspace.command.begin requires 'owner_starttime_ticks'")
        })?;
    if !crate::workspace::session_process_matches(owner_pid, Some(owner_starttime)) {
        bail!("workspace.command.begin owner process {owner_pid} is not alive or does not match its start time");
    }
    let metadata = workspace::workspace_metadata(&config.state_dir, sandbox, workspace)?;
    let token = services.activity.begin_external(
        &metadata.sandbox_id,
        &metadata.id,
        owner_pid,
        owner_starttime,
    );
    crate::workspace::touch_activity_registry_only(
        &config.state_dir,
        &metadata.sandbox_id,
        &metadata.id,
        crate::workspace::ActivityKind::Request,
    )?;
    Ok(json!({ "lease": token, "workspace_id": metadata.id }))
}

pub(in crate::daemon::dispatch) fn dispatch_workspace_command_end(
    params: &Value,
    services: &crate::daemon::services::DaemonServices,
) -> Result<Value> {
    let token = require_param_str(params, &["lease"])?;
    Ok(json!({ "released": services.activity.end_external(token) }))
}

pub(in crate::daemon::dispatch) fn dispatch_workspace_jobs_list(
    params: &Value,
    config: &DaemonConfig,
    services: &crate::daemon::services::DaemonServices,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let metadata = workspace::workspace_metadata(&config.state_dir, sandbox, workspace)?;
    services.activity.refresh_jobs_for_workspace(
        &config.state_dir,
        &metadata.sandbox_id,
        &metadata.id,
    )?;
    let jobs =
        workspace::jobs::list_for_workspace(&config.state_dir, &metadata.sandbox_id, &metadata.id)?;
    let jobs = match params.get("job").and_then(Value::as_str) {
        Some(job_id) => jobs.into_iter().filter(|job| job.id == job_id).collect(),
        None => jobs,
    };
    Ok(serde_json::to_value(jobs)?)
}

pub(in crate::daemon::dispatch) fn dispatch_workspace_exec(
    params: &Value,
    config: &DaemonConfig,
    services: &crate::daemon::services::DaemonServices,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let cwd = params.get("cwd").and_then(Value::as_str).unwrap_or("/home");
    let command = parse_string_array(params, "command")?;
    // Scrubbing is on unless the caller turns it off, so a caller that says
    // nothing gets the safe behaviour.
    let scrub = params.get("scrub").and_then(Value::as_bool).unwrap_or(true);

    let metadata = workspace::workspace_metadata(&config.state_dir, sandbox, workspace_selector)?;
    let _lease =
        services
            .activity
            .begin_request(&config.state_dir, &metadata.sandbox_id, &metadata.id)?;
    let background = params
        .get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let keep_running = params
        .get("keep_running")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ttl_secs = params.get("ttl_secs").and_then(Value::as_u64);
    if background {
        let job = workspace::spawn_workspace_command_detached(
            &config.state_dir,
            &metadata,
            cwd,
            &command,
            ttl_secs,
            keep_running,
            Arc::clone(&services.activity),
        )?;
        workspace::touch_activity(
            &config.state_dir,
            &metadata.sandbox_id,
            &metadata.id,
            workspace::ActivityKind::Process,
        )?;
        return Ok(serde_json::json!({
            "job_id": job.id,
            "status": job.status,
            "pid": job.pid,
        }));
    }
    let result = workspace::exec_workspace_command_with_options(
        &config.state_dir,
        sandbox,
        workspace_selector,
        cwd,
        &command,
        workspace::WorkspaceExecOptions { scrub, ttl_secs },
    )?;
    if let Some(job_id) = result.job_id.as_deref() {
        if let Ok(job) = workspace::jobs::load(&config.state_dir, job_id) {
            services.activity.update_job(job);
        }
    }
    workspace::touch_activity(
        &config.state_dir,
        &metadata.sandbox_id,
        &metadata.id,
        workspace::ActivityKind::Process,
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
    services: &crate::daemon::services::DaemonServices,
    client_stream: Option<&UnixStream>,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let src = require_param_str(params, &["src"])?;
    let dst = require_param_str(params, &["dst"])?;
    let direction = require_param_str(params, &["direction"])?;
    let gzip = params.get("gzip").and_then(Value::as_bool).unwrap_or(false);
    let metadata = workspace::workspace_metadata(&config.state_dir, sandbox, workspace)?;
    // A copy can run for minutes. Hold the same activity lease a command holds
    // for its whole transfer, so an idle sweep cannot stop the runtime halfway
    // through and leave the caller with a partially copied tree.
    let _lease =
        services
            .activity
            .begin_request(&config.state_dir, &metadata.sandbox_id, &metadata.id)?;
    // The workspace layer touches activity again so direct callers share the
    // same behaviour; this request already holds the lease that prevents an
    // idle sweep during the transfer.
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
