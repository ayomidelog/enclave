use super::*;

pub(super) fn parse_published_ports(
    params: &Value,
    key: &str,
) -> Result<Option<Vec<workspace::PublishedPortSpec>>> {
    let Some(_) = params.get(key) else {
        return Ok(None);
    };

    let raw_specs = parse_string_array(params, key)?;
    let mut specs = Vec::with_capacity(raw_specs.len());
    for raw in raw_specs {
        specs.push(workspace::PublishedPortSpec::parse(&raw)?);
    }
    Ok(Some(specs))
}

pub(super) fn ensure_workspace_ports_started(
    state_dir: &std::path::Path,
    metadata: &workspace::WorkspaceMetadata,
    port_publisher: &Arc<PortPublisher>,
) -> Result<workspace::WorkspaceMetadata> {
    if metadata.published_ports.is_empty() {
        return Ok(metadata.clone());
    }

    let workspace_ip = metadata.assigned_ip.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "workspace '{}' started without networking; cannot publish declared ports",
            metadata.id
        )
    })?;
    let runtime_pid = metadata.runtime_pid.ok_or_else(|| {
        anyhow::anyhow!(
            "workspace '{}' is running without a runtime pid; restart it before publishing ports",
            metadata.id
        )
    })?;

    if let Err(err) = port_publisher.apply_workspace_ports_strict(
        &metadata.sandbox_id,
        &metadata.id,
        runtime_pid,
        workspace_ip,
        &metadata.published_ports,
    ) {
        port_publisher.clear_workspace_ports(&metadata.sandbox_id, &metadata.id);
        let _ = workspace::stop_workspace(state_dir, &metadata.sandbox_id, &metadata.id);
        return Err(err);
    }

    Ok(metadata.clone())
}

pub(super) fn update_workspace_definition_with_runtime(
    state_dir: &std::path::Path,
    request: WorkspaceDefinitionUpdateRequest,
    port_publisher: &Arc<PortPublisher>,
) -> Result<workspace::WorkspaceMetadata> {
    let WorkspaceDefinitionUpdateRequest {
        sandbox,
        workspace_selector,
        auth_providers,
        env_tokens,
        published_ports,
        limits_update,
    } = request;
    let current = workspace::workspace_metadata(state_dir, sandbox, workspace_selector)?;
    let updated = workspace::update_workspace_definition(
        state_dir,
        sandbox,
        workspace_selector,
        auth_providers,
        env_tokens,
        published_ports.clone(),
        limits_update.clone(),
    )?;

    if !limits_update.is_empty() {
        if let Err(err) =
            workspace::sync_workspace_runtime_limits(state_dir, &updated.sandbox_id, &updated.id)
        {
            rollback_workspace_definition_update(state_dir, &current, port_publisher);
            return Err(err);
        }
    }

    if published_ports.is_none() {
        return workspace::workspace_metadata(state_dir, &updated.sandbox_id, &updated.id);
    }

    let apply_result = if updated.status.is_running() {
        let workspace_ip = updated.assigned_ip.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "workspace '{}' is running without networking; restart it before publishing ports",
                updated.id
            )
        })?;
        let runtime_pid = updated.runtime_pid.ok_or_else(|| {
            anyhow::anyhow!(
                "workspace '{}' is running without a runtime pid; restart it before publishing ports",
                updated.id
            )
        })?;
        port_publisher.apply_workspace_ports_strict(
            &updated.sandbox_id,
            &updated.id,
            runtime_pid,
            workspace_ip,
            &updated.published_ports,
        )
    } else {
        port_publisher.clear_workspace_ports(&updated.sandbox_id, &updated.id);
        Ok(Vec::new())
    };

    if let Err(err) = apply_result {
        rollback_workspace_definition_update(state_dir, &current, port_publisher);
        return Err(err);
    }

    workspace::workspace_metadata(state_dir, &updated.sandbox_id, &updated.id)
}

pub(super) struct WorkspaceDefinitionUpdateRequest<'a> {
    pub(super) sandbox: &'a str,
    pub(super) workspace_selector: &'a str,
    pub(super) auth_providers: Option<Vec<String>>,
    pub(super) env_tokens: Option<Vec<String>>,
    pub(super) published_ports: Option<Vec<workspace::PublishedPortSpec>>,
    pub(super) limits_update: workspace::WorkspaceLimitsUpdate,
}

pub(super) fn rollback_workspace_definition_update(
    state_dir: &std::path::Path,
    previous: &workspace::WorkspaceMetadata,
    port_publisher: &Arc<PortPublisher>,
) {
    if let Err(err) = workspace::update_workspace_definition(
        state_dir,
        &previous.sandbox_id,
        &previous.id,
        Some(previous.auth_providers.clone()),
        Some(previous.env_tokens.clone()),
        Some(previous.published_ports.clone()),
        workspace::WorkspaceLimitsUpdate {
            clear_tmp_on_restart: Some(previous.clear_tmp_on_restart),
            cpu_seconds: Some(previous.limits.cpu_seconds),
            cpu_percent: Some(previous.limits.cpu_percent),
            memory_bytes: Some(previous.limits.memory_bytes),
            max_processes: Some(previous.limits.max_processes),
            max_open_files: Some(previous.limits.max_open_files),
            disk_bytes: Some(previous.limits.disk_bytes),
        },
    ) {
        tracing::warn!(
            "failed to roll back workspace definition for {}: {err:#}",
            previous.id
        );
    }

    if let Err(err) =
        workspace::sync_workspace_runtime_limits(state_dir, &previous.sandbox_id, &previous.id)
    {
        tracing::warn!(
            "failed to restore runtime limits for {} after rollback: {err:#}",
            previous.id
        );
    }

    if previous.status.is_running() {
        if let Some(workspace_ip) = previous.assigned_ip.as_deref() {
            let Some(runtime_pid) = previous.runtime_pid else {
                port_publisher.clear_workspace_ports(&previous.sandbox_id, &previous.id);
                return;
            };
            if let Err(err) = port_publisher.apply_workspace_ports_strict(
                &previous.sandbox_id,
                &previous.id,
                runtime_pid,
                workspace_ip,
                &previous.published_ports,
            ) {
                tracing::warn!(
                    "failed to restore published ports for {} after rollback: {err:#}",
                    previous.id
                );
            }
        } else {
            port_publisher.clear_workspace_ports(&previous.sandbox_id, &previous.id);
        }
    } else {
        port_publisher.clear_workspace_ports(&previous.sandbox_id, &previous.id);
    }
}

pub(super) fn workspace_port_statuses(
    metadata: &workspace::WorkspaceMetadata,
    port_publisher: &Arc<PortPublisher>,
) -> Vec<workspace::PublishedPortStatus> {
    workspace::merge_published_port_statuses(
        &metadata.published_ports,
        &port_publisher.workspace_statuses(&metadata.sandbox_id, &metadata.id),
    )
}
