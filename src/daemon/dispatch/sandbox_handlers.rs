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
    // Creating a sandbox also starts it, so the response describes the whole
    // change: there was no sandbox, and there is now a running one.
    Ok(with_transition(
        serde_json::to_value(started)?,
        ABSENT,
        sandbox::SandboxStatus::Running.as_str(),
    ))
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
    let previous_state = sandbox_state_before(&config.state_dir, selector);
    let status = sandbox::sandbox_status(&config.state_dir, selector)?;
    if status.status == sandbox::SandboxStatus::Paused {
        let resumed = sandbox::resume_sandbox(&config.state_dir, selector)?;
        return Ok(with_transition(
            serde_json::to_value(resumed)?,
            previous_state,
            sandbox::SandboxStatus::Running.as_str(),
        ));
    }
    let metadata = sandbox::start_sandbox(&config.state_dir, selector)?;
    Ok(with_transition(
        serde_json::to_value(metadata)?,
        previous_state,
        sandbox::SandboxStatus::Running.as_str(),
    ))
}

pub(super) fn dispatch_sandbox_stop(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let previous_state = sandbox_state_before(&config.state_dir, selector);
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
    Ok(with_transition(
        serde_json::to_value(metadata)?,
        previous_state,
        sandbox::SandboxStatus::Stopped.as_str(),
    ))
}

pub(super) fn dispatch_sandbox_pause(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let previous_state = sandbox_state_before(&config.state_dir, selector);
    let metadata = sandbox::pause_sandbox(&config.state_dir, selector)?;
    let workspaces = workspace::list_workspaces(&config.state_dir, Some(selector))?;
    for workspace in workspaces {
        if workspace.status.is_running() {
            port_publisher.clear_workspace_ports(&workspace.sandbox_id, &workspace.id);
        }
    }
    Ok(with_transition(
        serde_json::to_value(metadata)?,
        previous_state,
        sandbox::SandboxStatus::Paused.as_str(),
    ))
}

pub(super) fn dispatch_sandbox_resume(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let previous_state = sandbox_state_before(&config.state_dir, selector);
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
    Ok(with_transition(
        json!({
            "sandbox": metadata,
            "port_failures": port_failures,
        }),
        previous_state,
        sandbox::SandboxStatus::Running.as_str(),
    ))
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
    let mode = parse_cleanup_mode(params)?;
    let previous_state = sandbox_state_before(&config.state_dir, selector);
    let status = sandbox::sandbox_status(&config.state_dir, selector)?;
    if status.status == sandbox::SandboxStatus::Paused {
        sandbox::resume_sandbox(&config.state_dir, selector)?;
    }
    let workspaces = workspace::list_workspaces(&config.state_dir, Some(selector))?;
    let failed = workspace::stop_running_workspaces_in_sandbox(&config.state_dir, selector)?;
    if !failed.is_empty() && !mode.is_force() {
        bail!(
            "sandbox destroy aborted because workspace cleanup did not complete: {}",
            failed.join("; ")
        );
    }
    for workspace in workspaces {
        port_publisher.clear_workspace_ports(&workspace.sandbox_id, &workspace.id);
    }
    let report = sandbox::destroy_sandbox_with_mode(&config.state_dir, selector, mode)?;
    // The record is gone by now, so the state before is the only remaining
    // description of what the destroy removed.
    Ok(with_transition(
        json!({
            "sandbox_id": report.sandbox_id,
            "mode": report.mode,
            "retained": report.retained,
            "workspace_stop_failures": failed,
        }),
        previous_state,
        ABSENT,
    ))
}

#[derive(Debug, serde::Serialize)]
pub(super) struct SandboxWipeReport {
    removed: Vec<String>,
    errors: Vec<String>,
    /// Resources a force wipe could not release, keyed by sandbox id.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    retained: std::collections::BTreeMap<String, Vec<workspace::RetainedResource>>,
}

pub(super) fn dispatch_sandbox_wipe(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let mode = parse_cleanup_mode(params)?;
    let sandboxes = sandbox::list_sandbox_items(&config.state_dir)?;
    let mut report = SandboxWipeReport {
        removed: Vec::new(),
        errors: Vec::new(),
        retained: std::collections::BTreeMap::new(),
    };
    for item in sandboxes {
        let sandbox_id = item.id;
        match dispatch_sandbox_destroy(
            &serde_json::json!({ "sandbox": sandbox_id, "force": mode.is_force() }),
            config,
            port_publisher,
        ) {
            Ok(value) => {
                let retained: Vec<workspace::RetainedResource> =
                    serde_json::from_value(value.get("retained").cloned().unwrap_or_default())?;
                if !retained.is_empty() {
                    report.retained.insert(sandbox_id.clone(), retained);
                }
                report.removed.push(sandbox_id);
            }
            Err(error) => report.errors.push(format!("{sandbox_id}: {error:#}")),
        }
    }
    Ok(serde_json::to_value(report)?)
}

pub(super) fn dispatch_sandbox_list(config: &DaemonConfig) -> Result<Value> {
    let sandboxes = sandbox::list_sandbox_items(&config.state_dir)?;
    Ok(serde_json::to_value(sandboxes)?)
}

pub(super) fn dispatch_sandbox_remove(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let previous_state = sandbox_state_before(&config.state_dir, selector);
    let removed = sandbox::destroy_sandbox(&config.state_dir, selector)?;
    Ok(with_transition(
        json!({ "removed": removed }),
        previous_state,
        ABSENT,
    ))
}
