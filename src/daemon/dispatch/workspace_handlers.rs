use super::*;

pub(super) fn dispatch_workspace_start_many(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let specs = params
        .get("workspaces")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("missing 'workspaces' array"))?
        .clone();
    if specs.is_empty() {
        return Ok(json!([]));
    }

    let worker_count = std::env::var("ENCLAVE_UP_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=64).contains(value))
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|parallelism| parallelism.get().max(1))
                .unwrap_or(4)
        })
        .min(specs.len());
    let queue = std::sync::Arc::new(std::sync::Mutex::new(
        std::collections::VecDeque::from_iter(specs.into_iter().enumerate()),
    ));
    let result_count = queue.lock().map(|queue| queue.len()).unwrap_or(0);
    let results = std::sync::Arc::new(std::sync::Mutex::new(
        (0..result_count)
            .map(|_| None::<Result<Value>>)
            .collect::<Vec<_>>(),
    ));

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = std::sync::Arc::clone(&queue);
            let results = std::sync::Arc::clone(&results);
            let config = config.clone();
            let port_publisher = Arc::clone(port_publisher);
            scope.spawn(move || loop {
                let Some((index, spec)) = queue.lock().ok().and_then(|mut queue| queue.pop_front())
                else {
                    return;
                };
                let result = dispatch_workspace_start_many_item(&spec, &config, &port_publisher);
                if let Ok(mut results) = results.lock() {
                    results[index] = Some(result);
                }
            });
        }
    });

    let results = Arc::try_unwrap(results)
        .map_err(|_| anyhow::anyhow!("workspace batch result ownership remained active"))?
        .into_inner()
        .map_err(|_| anyhow::anyhow!("workspace batch result lock poisoned"))?;
    let mut output = Vec::with_capacity(results.len());
    for (index, result) in results.into_iter().enumerate() {
        let result =
            result.ok_or_else(|| anyhow::anyhow!("workspace batch item did not complete"))?;
        output.push(match result {
            Ok(value) => json!({ "index": index, "ok": true, "result": value }),
            Err(error) => json!({
                "index": index,
                "ok": false,
                "error": format!("{error:#}"),
            }),
        });
    }
    Ok(Value::Array(output))
}

pub(super) fn dispatch_workspace_start_many_item(
    spec: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let started = match dispatch_workspace_create(spec, config, port_publisher) {
        Ok(result) => result,
        Err(error) => {
            let sandbox = require_param_str(spec, &["sandbox_id"])?;
            let workspace = require_param_str(spec, &["name"])?;
            let exists = workspace::list_workspaces(&config.state_dir, Some(sandbox))?
                .iter()
                .any(|item| item.name == workspace || item.id == workspace);
            if !exists {
                return Err(error);
            }
            let update = existing_workspace_update(spec, sandbox, workspace);
            dispatch_workspace_update(&update, config, port_publisher)?;
            dispatch_workspace_target(
                &json!({ "sandbox": sandbox, "workspace": workspace }),
                config,
                "start",
                port_publisher,
            )?
        }
    };

    if let Some(run_command) = spec.get("run").and_then(Value::as_str) {
        let metadata: workspace::WorkspaceMetadata = serde_json::from_value(started.clone())?;
        workspace::spawn_workspace_command_detached(
            &metadata,
            "/home",
            &["sh".to_string(), "-c".to_string(), run_command.to_string()],
        )
        .with_context(|| {
            format!(
                "failed to launch run command for workspace '{}'",
                metadata.name
            )
        })?;
    }
    Ok(started)
}

/// Translate a `workspace.start_many` item into a `workspace.update` request
/// for a workspace that already exists.
///
/// The two requests share parameter names but not semantics. A start item
/// carries the declared definition, including `disk_mb`, while an update
/// cannot change an existing disk allocation at all. Forwarding the declared
/// size would make every `up` after a `down` fail for quota-backed workspaces.
pub(super) fn existing_workspace_update(spec: &Value, sandbox: &str, workspace: &str) -> Value {
    let mut update = spec.clone();
    let Some(object) = update.as_object_mut() else {
        return update;
    };
    for key in [
        "cpu_seconds",
        "cpu_percent",
        "memory_mb",
        "max_procs",
        "max_open_files",
        "path",
    ] {
        if object.get(key).is_some_and(Value::is_null) {
            object.remove(key);
        }
    }
    object.remove("disk_mb");
    object.insert("sandbox".to_string(), Value::String(sandbox.to_string()));
    object.insert(
        "workspace".to_string(),
        Value::String(workspace.to_string()),
    );
    update
}

pub(super) fn dispatch_workspace_create(
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
    Ok(serde_json::to_value(started)?)
}

pub(super) fn dispatch_workspace_target(
    params: &Value,
    config: &DaemonConfig,
    operation: &str,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;

    match operation {
        "start" => {
            let metadata = workspace::start_workspace_with_security(
                &config.state_dir,
                sandbox,
                workspace_selector,
                config.workspace_apparmor_profile.as_deref(),
                config.workspace_selinux_label.as_deref(),
            )?;
            let metadata =
                ensure_workspace_ports_started(&config.state_dir, &metadata, port_publisher)?;
            Ok(serde_json::to_value(metadata)?)
        }
        "stop" => {
            let metadata =
                workspace::stop_workspace(&config.state_dir, sandbox, workspace_selector)?;
            port_publisher.clear_workspace_ports(&metadata.sandbox_id, &metadata.id);
            Ok(serde_json::to_value(metadata)?)
        }
        "destroy" => {
            let metadata_before =
                workspace::workspace_metadata(&config.state_dir, sandbox, workspace_selector)?;
            let removed =
                workspace::destroy_workspace(&config.state_dir, sandbox, workspace_selector)?;
            port_publisher.clear_workspace_ports(&metadata_before.sandbox_id, &metadata_before.id);
            Ok(json!({ "removed": removed, "sandbox": sandbox }))
        }
        "status" => {
            let metadata =
                workspace::workspace_metadata(&config.state_dir, sandbox, workspace_selector)?;
            let report = workspace::workspace_status(
                &config.state_dir,
                sandbox,
                workspace_selector,
                &port_publisher.workspace_statuses(&metadata.sandbox_id, &metadata.id),
            )?;
            Ok(serde_json::to_value(report)?)
        }
        "stats" => {
            let report =
                workspace::workspace_stats(&config.state_dir, sandbox, workspace_selector)?;
            Ok(serde_json::to_value(report)?)
        }
        "remove" => {
            let metadata_before =
                workspace::workspace_metadata(&config.state_dir, sandbox, workspace_selector)?;
            workspace::remove_workspace(&config.state_dir, sandbox, workspace_selector)?;
            port_publisher.clear_workspace_ports(&metadata_before.sandbox_id, &metadata_before.id);
            Ok(json!({
                "removed": workspace_selector,
                "sandbox": sandbox,
            }))
        }
        "runtime" => {
            let result =
                workspace::workspace_runtime_info(&config.state_dir, sandbox, workspace_selector)?;
            Ok(serde_json::to_value(result)?)
        }
        "snapshot_list" => {
            let result = workspace::list_workspace_snapshots(
                &config.state_dir,
                sandbox,
                workspace_selector,
            )?;
            Ok(serde_json::to_value(result)?)
        }
        _ => bail!("unknown workspace target operation '{}'", operation),
    }
}

pub(super) fn dispatch_workspace_list(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let sandbox_id = params.get("sandbox_id").and_then(Value::as_str);
    if let Some(selector) = sandbox_id {
        let items = workspace::list_workspace_items(&config.state_dir, selector)?;
        return Ok(serde_json::to_value(items)?);
    }
    let workspaces = workspace::list_workspaces(&config.state_dir, None)?;
    Ok(serde_json::to_value(workspaces)?)
}

pub(super) fn dispatch_workspace_update(
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

pub(super) fn dispatch_workspace_resize(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let new_disk_bytes = parse_required_disk_bytes(params)?;
    let current = workspace::workspace_metadata(&config.state_dir, sandbox, workspace_selector)?;
    if current.status == workspace::WorkspaceStatus::Running
        && current
            .limits
            .disk_bytes
            .is_some_and(|bytes| bytes < new_disk_bytes)
    {
        port_publisher.clear_workspace_ports(&current.sandbox_id, &current.id);
    }

    let result = workspace::resize_workspace_disk_with_security(
        &config.state_dir,
        sandbox,
        workspace_selector,
        new_disk_bytes,
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

pub(super) fn dispatch_workspace_exec(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let cwd = params.get("cwd").and_then(Value::as_str).unwrap_or("/home");
    let command = parse_string_array(params, "command")?;

    let result = workspace::exec_workspace_command(
        &config.state_dir,
        sandbox,
        workspace_selector,
        cwd,
        &command,
    )?;
    Ok(serde_json::to_value(result)?)
}

pub(super) fn dispatch_workspace_cp(
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

pub(super) fn dispatch_workspace_logs(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let tail = params
        .get("tail")
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    let offset = params.get("offset").and_then(Value::as_u64);
    let result =
        workspace::workspace_logs(&config.state_dir, sandbox, workspace_selector, tail, offset)?;
    Ok(serde_json::to_value(result)?)
}
