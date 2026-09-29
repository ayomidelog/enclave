//! Creating and starting a workspace in one request.

use super::*;

pub(in crate::daemon::dispatch) fn dispatch_workspace_create(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let sandbox_id = require_param_str(params, &["sandbox_id"])?;
    let name = require_param_str(params, &["name"])?;
    let path = params.get("path").and_then(Value::as_str);
    let limits = parse_workspace_limits_create(params)?;
    let auth_providers = params
        .get("auth")
        .map(|_| parse_string_array(params, "auth"))
        .transpose()?
        .unwrap_or_default();
    let owner = params
        .get("owner")
        .and_then(Value::as_str)
        .map(str::to_string);
    let env_tokens = params
        .get("env_tokens")
        .map(|_| parse_string_array(params, "env_tokens"))
        .transpose()?
        .unwrap_or_default();
    let published_ports = parse_published_ports(params, "ports")?.unwrap_or_default();
    let clear_tmp_on_restart = params
        .get("clear_tmp_on_restart")
        .map(|_| parse_optional_bool_field(params, "clear_tmp_on_restart"))
        .transpose()?
        .flatten()
        .unwrap_or(false);
    let metadata = workspace::create_workspace_with_options(
        &config.state_dir,
        sandbox_id,
        name,
        workspace::WorkspaceCreateOptions {
            limits,
            home_mount_source: path.map(str::to_string),
            auth_providers,
            owner,
            env_tokens,
            published_ports,
            clear_tmp_on_restart,
        },
    )?;
    let started = workspace::start_workspace_with_security(
        &config.state_dir,
        sandbox_id,
        &metadata.id,
        config.workspace_apparmor_profile.as_deref(),
        config.workspace_selinux_label.as_deref(),
    )?;
    let started = ensure_workspace_ports_started(&config.state_dir, &started, port_publisher)?;
    // Creating a workspace also starts it, so the response describes the whole
    // change: there was no workspace, and there is now a running one.
    Ok(with_transition(
        serde_json::to_value(started)?,
        ABSENT,
        WorkspaceStatus::Running.as_str(),
    ))
}
