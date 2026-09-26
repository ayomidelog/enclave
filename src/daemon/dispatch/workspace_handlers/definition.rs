//! Reading and changing a workspace definition: list, update, resize.

use super::*;

pub(in crate::daemon::dispatch) fn dispatch_workspace_list(
    params: &Value,
    config: &DaemonConfig,
) -> Result<Value> {
    let sandbox_id = params.get("sandbox_id").and_then(Value::as_str);
    if let Some(selector) = sandbox_id {
        let items = workspace::list_workspace_items(&config.state_dir, selector)?;
        return Ok(serde_json::to_value(items)?);
    }
    let workspaces = workspace::list_workspaces(&config.state_dir, None)?;
    Ok(serde_json::to_value(workspaces)?)
}

pub(in crate::daemon::dispatch) fn dispatch_workspace_update(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let auth_providers = params
        .get("auth")
        .map(|_| parse_string_array(params, "auth"))
        .transpose()?;
    let env_tokens = params
        .get("env_tokens")
        .map(|_| parse_string_array(params, "env_tokens"))
        .transpose()?;
    let published_ports = parse_published_ports(params, "ports")?;
    let limits = parse_workspace_limits_update(params)?;
    update_workspace_definition_with_runtime(
        &config.state_dir,
        WorkspaceDefinitionUpdateRequest {
            sandbox,
            workspace_selector,
            auth_providers,
            env_tokens,
            published_ports,
            limits_update: limits,
        },
        port_publisher,
    )?;
    Ok(json!({"updated": true}))
}

pub(in crate::daemon::dispatch) fn dispatch_workspace_resize(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let new_disk_bytes = parse_optional_disk_bytes(params)?;
    let new_memory_bytes = parse_optional_memory_bytes(params)?;
    if new_disk_bytes.is_none() && new_memory_bytes.is_none() {
        bail!("workspace.resize requires 'disk_mb' or 'memory_mb'");
    }
    let current = workspace::workspace_metadata(&config.state_dir, sandbox, workspace_selector)?;
    // A disk resize stops and restarts a running workspace, which drops its published
    // ports until the relaunch puts them back. They are withdrawn before the stop so
    // the host never has a listener forwarding to a runtime that is gone. A memory
    // resize does not interrupt anything, so it leaves the ports alone.
    let disk_changes = new_disk_bytes.is_some_and(|bytes| current.limits.disk_bytes != Some(bytes));
    if current.status.is_running() && disk_changes {
        port_publisher.clear_workspace_ports(&current.sandbox_id, &current.id);
    }

    let result = workspace::resize_workspace_with_security(
        &config.state_dir,
        sandbox,
        workspace_selector,
        new_disk_bytes,
        new_memory_bytes,
        config.workspace_apparmor_profile.as_deref(),
        config.workspace_selinux_label.as_deref(),
    )?;
    if result.restarted {
        let metadata =
            workspace::workspace_metadata(&config.state_dir, sandbox, &result.workspace_id)?;
        ensure_workspace_ports_started(&config.state_dir, &metadata, port_publisher)?;
    }
    Ok(serde_json::to_value(result)?)
}
