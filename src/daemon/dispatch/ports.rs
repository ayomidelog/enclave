use super::*;

pub(super) fn dispatch_workspace_port_publish(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let spec_raw = require_param_str(params, &["spec"])?;
    let spec = workspace::PublishedPortSpec::parse(spec_raw)?;

    let current = workspace::workspace_metadata(&config.state_dir, sandbox, workspace_selector)?;
    let mut desired = current.published_ports.clone();
    if !desired.contains(&spec) {
        desired.push(spec);
    }
    crate::workspace::validate_published_ports(&desired)?;

    let updated = update_workspace_definition_with_runtime(
        &config.state_dir,
        WorkspaceDefinitionUpdateRequest {
            sandbox,
            workspace_selector,
            auth_providers: None,
            owner: None,
            env_tokens: None,
            published_ports: Some(desired),
            limits_update: workspace::WorkspaceLimitsUpdate::default(),
        },
        port_publisher,
    )?;
    Ok(serde_json::to_value(workspace_port_statuses(
        &updated,
        port_publisher,
    ))?)
}

pub(super) fn dispatch_workspace_port_unpublish(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let binding_raw = require_param_str(params, &["binding"])?;
    let binding = workspace::PublishedPortBinding::parse(binding_raw)?;

    let current = workspace::workspace_metadata(&config.state_dir, sandbox, workspace_selector)?;
    let desired = current
        .published_ports
        .iter()
        .filter(|spec| spec.binding() != binding)
        .cloned()
        .collect::<Vec<_>>();
    if desired.len() == current.published_ports.len() {
        bail!(
            "workspace '{}' has no published port bound at {}:{}",
            current.id,
            binding.host_ip,
            binding.host_port
        );
    }

    let updated = update_workspace_definition_with_runtime(
        &config.state_dir,
        WorkspaceDefinitionUpdateRequest {
            sandbox,
            workspace_selector,
            auth_providers: None,
            owner: None,
            env_tokens: None,
            published_ports: Some(desired),
            limits_update: workspace::WorkspaceLimitsUpdate::default(),
        },
        port_publisher,
    )?;
    Ok(serde_json::to_value(workspace_port_statuses(
        &updated,
        port_publisher,
    ))?)
}

pub(super) fn dispatch_workspace_port_list(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let metadata = workspace::workspace_metadata(&config.state_dir, sandbox, workspace_selector)?;
    Ok(serde_json::to_value(workspace_port_statuses(
        &metadata,
        port_publisher,
    ))?)
}
