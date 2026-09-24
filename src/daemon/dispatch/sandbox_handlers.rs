use super::*;

pub(super) fn dispatch_sandbox_create(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let name = require_param_str(params, &["name"])?;
    let suite = params
        .get("suite")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_DEBIAN_SUITE);
    let mirror = params
        .get("mirror")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_DEBIAN_MIRROR);
    let method: BootstrapMethod = params
        .get("bootstrap_method")
        .and_then(Value::as_str)
        .unwrap_or("debootstrap")
        .parse()?;
    let limits = parse_sandbox_limits_create(params)?;

    let metadata = sandbox::create_sandbox_with_options(
        &config.state_dir,
        &config.debootstrap_binary,
        name,
        suite,
        mirror,
        &method,
        sandbox::SandboxCreateOptions { limits },
    )?;
    let started = sandbox::start_sandbox(&config.state_dir, &metadata.id)?;
    Ok(serde_json::to_value(started)?)
}

pub(super) fn dispatch_sandbox_update(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let limits = parse_sandbox_limits_update(params)?;
    if limits.is_empty() {
        bail!("sandbox.update requires at least one limit field");
    }
    let updated = sandbox::update_sandbox_limits(&config.state_dir, selector, &limits)?;
    crate::workspace::sync_sandbox_runtime_limits(&config.state_dir, &updated.id)?;
    Ok(serde_json::to_value(updated)?)
}

pub(super) fn dispatch_sandbox_start(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let status = sandbox::sandbox_status(&config.state_dir, selector)?;
    if status.status == sandbox::SandboxStatus::Paused {
        let resumed = sandbox::resume_sandbox(&config.state_dir, selector)?;
        return Ok(serde_json::to_value(resumed)?);
    }
    let metadata = sandbox::start_sandbox(&config.state_dir, selector)?;
    Ok(serde_json::to_value(metadata)?)
}

pub(super) fn dispatch_sandbox_stop(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let status = sandbox::sandbox_status(&config.state_dir, selector)?;
    if status.status == sandbox::SandboxStatus::Paused {
        sandbox::resume_sandbox(&config.state_dir, selector)?;
    }
    let workspaces = workspace::list_workspaces(&config.state_dir, Some(selector))?;
    let failed = workspace::stop_running_workspaces_in_sandbox(&config.state_dir, selector)?;
    if !failed.is_empty() {
        bail!(
            "sandbox stop: failed to stop {} workspace(s): {}",
            failed.len(),
            failed.join(", ")
        );
    }
    for workspace in workspaces {
        if workspace.status.is_running() {
            port_publisher.clear_workspace_ports(&workspace.sandbox_id, &workspace.id);
        }
    }
    let metadata = sandbox::stop_sandbox(&config.state_dir, selector)?;
    Ok(serde_json::to_value(metadata)?)
}

pub(super) fn dispatch_sandbox_pause(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let metadata = sandbox::pause_sandbox(&config.state_dir, selector)?;
    let workspaces = workspace::list_workspaces(&config.state_dir, Some(selector))?;
    for workspace in workspaces {
        if workspace.status.is_running() {
            port_publisher.clear_workspace_ports(&workspace.sandbox_id, &workspace.id);
        }
    }
    Ok(serde_json::to_value(metadata)?)
}

pub(super) fn dispatch_sandbox_resume(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let metadata = sandbox::resume_sandbox(&config.state_dir, selector)?;
    let workspaces = workspace::list_workspaces(&config.state_dir, Some(selector))?;
    let mut port_failures = Vec::new();
    for workspace in workspaces {
        if workspace.status.is_running() && !workspace.published_ports.is_empty() {
            let Some(ip) = workspace.assigned_ip.as_deref() else {
                port_failures.push(format!("{}: missing workspace IP", workspace.name));
                continue;
            };
            let Some(pid) = workspace.runtime_pid else {
                port_failures.push(format!("{}: missing runtime PID", workspace.name));
                continue;
            };
            let statuses = port_publisher.reconcile_workspace_ports(
                &workspace.sandbox_id,
                &workspace.id,
                pid,
                ip,
                &workspace.published_ports,
            )?;
            for status in statuses {
                if let workspace::PublishedPortState::Failed = status.state {
                    port_failures.push(format!(
                        "{}: {}",
                        workspace.name,
                        status
                            .error
                            .unwrap_or_else(|| "port publication failed".to_string())
                    ));
                }
            }
        }
    }
    Ok(json!({
        "sandbox": metadata,
        "port_failures": port_failures,
    }))
}

pub(super) fn dispatch_sandbox_status(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let report = sandbox::sandbox_status(&config.state_dir, selector)?;
    Ok(serde_json::to_value(report)?)
}

pub(super) fn dispatch_sandbox_destroy(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let status = sandbox::sandbox_status(&config.state_dir, selector)?;
    if status.status == sandbox::SandboxStatus::Paused {
        sandbox::resume_sandbox(&config.state_dir, selector)?;
    }
    let workspaces = workspace::list_workspaces(&config.state_dir, Some(selector))?;
    let failed = workspace::stop_running_workspaces_in_sandbox(&config.state_dir, selector)?;
    if !failed.is_empty() {
        bail!(
            "sandbox destroy aborted because workspace cleanup did not complete: {}",
            failed.join("; ")
        );
    }
    for workspace in workspaces {
        port_publisher.clear_workspace_ports(&workspace.sandbox_id, &workspace.id);
    }
    let removed = sandbox::destroy_sandbox(&config.state_dir, selector)?;
    Ok(json!({ "removed": removed }))
}

#[derive(Debug, serde::Serialize)]
pub(super) struct SandboxWipeReport {
    removed: Vec<String>,
    errors: Vec<String>,
}

pub(super) fn dispatch_sandbox_wipe(
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let sandboxes = sandbox::list_sandbox_items(&config.state_dir)?;
    let mut report = SandboxWipeReport {
        removed: Vec::new(),
        errors: Vec::new(),
    };
    for item in sandboxes {
        match dispatch_sandbox_destroy(
            &serde_json::json!({"sandbox": item.id}),
            config,
            port_publisher,
        ) {
            Ok(_) => report.removed.push(item.id),
            Err(error) => report.errors.push(format!("{}: {error:#}", item.id)),
        }
    }
    Ok(serde_json::to_value(report)?)
}
