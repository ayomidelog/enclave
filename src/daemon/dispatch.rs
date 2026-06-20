use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::network::publish::PortPublisher;
use crate::policy;
use crate::registry;
use crate::sandbox::{self, BootstrapMethod, DEFAULT_DEBIAN_MIRROR, DEFAULT_DEBIAN_SUITE};
use crate::workspace;

use super::DaemonConfig;

fn require_param_str<'a>(params: &'a Value, keys: &[&str]) -> Result<&'a str> {
    for key in keys {
        if let Some(value) = params.get(key).and_then(Value::as_str) {
            return Ok(value);
        }
    }
    bail!("missing '{}'", keys[0])
}

fn parse_string_array(params: &Value, key: &str) -> Result<Vec<String>> {
    let values = params
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("missing '{}' array", key))?;

    let mut out = Vec::with_capacity(values.len());
    for (idx, value) in values.iter().enumerate() {
        let item = value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("'{}[{}]' must be a string", key, idx))?;
        out.push(item.to_string());
    }

    Ok(out)
}

fn parse_optional_u64_field(params: &Value, key: &str) -> Result<Option<Option<u64>>> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(Some(None));
    }
    let parsed = value
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("'{}' must be an unsigned integer", key))?;
    Ok(Some(Some(parsed)))
}

fn parse_optional_f64_field(params: &Value, key: &str) -> Result<Option<Option<f64>>> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(Some(None));
    }
    let parsed = value
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("'{}' must be a number", key))?;
    Ok(Some(Some(parsed)))
}

fn parse_workspace_limits_create(params: &Value) -> Result<workspace::WorkspaceLimits> {
    let cpu_seconds = params.get("cpu_seconds").and_then(Value::as_u64);
    let cpu_percent = params.get("cpu_percent").and_then(Value::as_f64);
    let memory_mb = params.get("memory_mb").and_then(Value::as_u64);
    let max_procs = params.get("max_procs").and_then(Value::as_u64);
    let max_open_files = params.get("max_open_files").and_then(Value::as_u64);
    let disk_mb = params.get("disk_mb").and_then(Value::as_u64);
    let limits = workspace::WorkspaceLimits {
        cpu_seconds,
        cpu_percent,
        memory_bytes: memory_mb.map(|v| v.saturating_mul(1024 * 1024)),
        max_processes: max_procs,
        max_open_files,
        disk_bytes: disk_mb.map(|v| v.saturating_mul(1024 * 1024)),
    };
    limits.validate()?;
    Ok(limits)
}

fn parse_workspace_limits_update(params: &Value) -> Result<workspace::WorkspaceLimitsUpdate> {
    Ok(workspace::WorkspaceLimitsUpdate {
        cpu_seconds: parse_optional_u64_field(params, "cpu_seconds")?,
        cpu_percent: parse_optional_f64_field(params, "cpu_percent")?,
        memory_bytes: parse_optional_u64_field(params, "memory_mb")?
            .map(|value| value.map(|mb| mb.saturating_mul(1024 * 1024))),
        max_processes: parse_optional_u64_field(params, "max_procs")?,
        max_open_files: parse_optional_u64_field(params, "max_open_files")?,
        disk_bytes: parse_optional_u64_field(params, "disk_mb")?
            .map(|value| value.map(|mb| mb.saturating_mul(1024 * 1024))),
    })
}

fn parse_sandbox_limits_create(params: &Value) -> Result<sandbox::SandboxLimits> {
    let limits = sandbox::SandboxLimits {
        cpu_percent: params.get("cpu_percent").and_then(Value::as_f64),
        memory_bytes: params
            .get("memory_mb")
            .and_then(Value::as_u64)
            .map(|v| v.saturating_mul(1024 * 1024)),
        max_processes: params.get("max_procs").and_then(Value::as_u64),
    };
    limits.validate()?;
    Ok(limits)
}

fn parse_sandbox_limits_update(params: &Value) -> Result<sandbox::SandboxLimitsUpdate> {
    Ok(sandbox::SandboxLimitsUpdate {
        cpu_percent: parse_optional_f64_field(params, "cpu_percent")?,
        memory_bytes: parse_optional_u64_field(params, "memory_mb")?
            .map(|value| value.map(|mb| mb.saturating_mul(1024 * 1024))),
        max_processes: parse_optional_u64_field(params, "max_procs")?,
    })
}

pub(crate) fn dispatch(
    request: crate::protocol::Request,
    config: &DaemonConfig,
    shutdown: &Arc<AtomicBool>,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let action = Action::parse(&request.action)?;
    match action {
        Action::Ping => Ok(json!({"status": "pong"})),
        Action::DaemonHealth => Ok(json!({
            "status": "ok",
            "pid": std::process::id(),
            "state_dir": config.state_dir.to_string_lossy(),
            "socket_path": config.socket_path.to_string_lossy(),
        })),
        Action::DaemonDoctor => {
            let report = crate::doctor::run_doctor(&config.state_dir)?;
            Ok(serde_json::to_value(report)?)
        }
        Action::Init => {
            sandbox::init_storage(&config.state_dir)?;
            Ok(json!({
                "state_dir": config.state_dir.to_string_lossy(),
                "socket_path": config.socket_path.to_string_lossy(),
            }))
        }
        Action::SandboxCreate => dispatch_sandbox_create(&request.params, config),
        Action::SandboxUpdate => dispatch_sandbox_update(&request.params, config),
        Action::SandboxStart => dispatch_sandbox_start(&request.params, config),
        Action::SandboxStop => dispatch_sandbox_stop(&request.params, config, port_publisher),
        Action::SandboxStatus => dispatch_sandbox_status(&request.params, config),
        Action::SandboxDestroy => dispatch_sandbox_destroy(&request.params, config, port_publisher),
        Action::SandboxList => {
            let sandboxes = sandbox::list_sandbox_items(&config.state_dir)?;
            Ok(serde_json::to_value(sandboxes)?)
        }
        Action::SandboxRemove => {
            let selector = require_param_str(&request.params, &["sandbox", "sandbox_id"])?;
            let removed = sandbox::destroy_sandbox(&config.state_dir, selector)?;
            Ok(json!({ "removed": removed }))
        }
        Action::SandboxExecSetup => {
            let selector = require_param_str(&request.params, &["sandbox", "sandbox_id"])?;
            let command = require_param_str(&request.params, &["command"])?;
            sandbox::exec_setup_command(&config.state_dir, selector, command)
        }
        Action::ProcessList => {
            let entries = workspace::list_process_status(&config.state_dir)?;
            Ok(serde_json::to_value(entries)?)
        }
        Action::WorkspaceCreate => {
            dispatch_workspace_create(&request.params, config, port_publisher)
        }
        Action::WorkspaceStart => {
            dispatch_workspace_target(&request.params, config, "start", port_publisher)
        }
        Action::WorkspaceStop => {
            dispatch_workspace_target(&request.params, config, "stop", port_publisher)
        }
        Action::WorkspaceDestroy => {
            dispatch_workspace_target(&request.params, config, "destroy", port_publisher)
        }
        Action::WorkspaceStatus => {
            dispatch_workspace_target(&request.params, config, "status", port_publisher)
        }
        Action::WorkspaceStats => {
            dispatch_workspace_target(&request.params, config, "stats", port_publisher)
        }
        Action::WorkspaceStatsList => {
            let stats = workspace::list_running_workspace_stats(&config.state_dir)?;
            Ok(serde_json::to_value(stats)?)
        }
        Action::WorkspaceList => dispatch_workspace_list(&request.params, config),
        Action::WorkspaceRemove => {
            dispatch_workspace_target(&request.params, config, "remove", port_publisher)
        }
        Action::WorkspaceUpdate => {
            dispatch_workspace_update(&request.params, config, port_publisher)
        }
        Action::WorkspaceExec => dispatch_workspace_exec(&request.params, config),
        Action::WorkspacePortPublish => {
            dispatch_workspace_port_publish(&request.params, config, port_publisher)
        }
        Action::WorkspacePortUnpublish => {
            dispatch_workspace_port_unpublish(&request.params, config, port_publisher)
        }
        Action::WorkspacePortList => {
            dispatch_workspace_port_list(&request.params, config, port_publisher)
        }
        Action::WorkspaceRuntime => {
            dispatch_workspace_target(&request.params, config, "runtime", port_publisher)
        }
        Action::WorkspaceLogs => dispatch_workspace_logs(&request.params, config),
        Action::WorkspaceSnapshot => dispatch_workspace_snapshot(&request.params, config),
        Action::WorkspaceSnapshotList => {
            dispatch_workspace_target(&request.params, config, "snapshot_list", port_publisher)
        }
        Action::WorkspaceRestore => dispatch_workspace_restore(&request.params, config),
        Action::WorkspaceSnapshotGc => dispatch_workspace_snapshot_gc(&request.params, config),
        Action::RegistryRepair => {
            let strict = request
                .params
                .get("strict")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let report = registry::repair_registry(&config.state_dir, strict)?;
            Ok(serde_json::to_value(report)?)
        }
        Action::PolicyGet => {
            let current = policy::load_policy(&config.state_dir)?;
            Ok(serde_json::to_value(current)?)
        }
        Action::PolicySetDefault => {
            let default_allow = request
                .params
                .get("default_allow")
                .and_then(Value::as_bool)
                .ok_or_else(|| anyhow::anyhow!("missing 'default_allow'"))?;
            let updated = policy::set_default_allow(&config.state_dir, default_allow)?;
            Ok(serde_json::to_value(updated)?)
        }
        Action::PolicyAllow => dispatch_policy_rule(&request.params, config, true),
        Action::PolicyDeny => dispatch_policy_rule(&request.params, config, false),
        Action::PolicyClear => {
            let uid = request
                .params
                .get("uid")
                .and_then(Value::as_u64)
                .map(|v| v as u32);
            let updated = policy::clear_rules(&config.state_dir, uid)?;
            Ok(serde_json::to_value(updated)?)
        }
        Action::Shutdown => {
            shutdown.store(true, Ordering::SeqCst);
            super::SIGNAL_SHUTDOWN.store(true, Ordering::SeqCst);
            Ok(json!({"status": "shutting_down"}))
        }
    }
}

fn dispatch_sandbox_create(params: &Value, config: &DaemonConfig) -> Result<Value> {
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

fn dispatch_sandbox_update(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let limits = parse_sandbox_limits_update(params)?;
    if limits.is_empty() {
        bail!("sandbox.update requires at least one limit field");
    }
    let updated = sandbox::update_sandbox_limits(&config.state_dir, selector, &limits)?;
    crate::workspace::sync_sandbox_runtime_limits(&config.state_dir, &updated.id)?;
    Ok(serde_json::to_value(updated)?)
}

fn dispatch_sandbox_start(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let metadata = sandbox::start_sandbox(&config.state_dir, selector)?;
    Ok(serde_json::to_value(metadata)?)
}

fn dispatch_sandbox_stop(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspaces = workspace::list_workspaces(&config.state_dir, Some(selector))?;
    let mut failed: Vec<String> = Vec::new();
    for ws in workspaces {
        if ws.status == crate::workspace::WorkspaceStatus::Running {
            port_publisher.clear_workspace_ports(&ws.sandbox_id, &ws.id);
            if let Err(err) = workspace::stop_workspace(&config.state_dir, selector, &ws.id) {
                tracing::warn!(
                    "failed to stop workspace {} during sandbox stop: {err:#}",
                    ws.id
                );
                failed.push(ws.id.clone());
            }
        }
    }
    if !failed.is_empty() {
        bail!(
            "sandbox stop: failed to stop {} workspace(s): {}",
            failed.len(),
            failed.join(", ")
        );
    }
    let metadata = sandbox::stop_sandbox(&config.state_dir, selector)?;
    Ok(serde_json::to_value(metadata)?)
}

fn dispatch_sandbox_status(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let report = sandbox::sandbox_status(&config.state_dir, selector)?;
    Ok(serde_json::to_value(report)?)
}

fn dispatch_sandbox_destroy(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspaces = workspace::list_workspaces(&config.state_dir, Some(selector))?;
    for ws in workspaces {
        port_publisher.clear_workspace_ports(&ws.sandbox_id, &ws.id);
        if let Err(err) = workspace::stop_workspace(&config.state_dir, selector, &ws.id) {
            tracing::warn!(
                "failed to stop workspace {} during sandbox destroy: {err:#}",
                ws.id
            );
        }
    }
    let removed = sandbox::destroy_sandbox(&config.state_dir, selector)?;
    Ok(json!({ "removed": removed }))
}

fn dispatch_workspace_create(
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

fn dispatch_workspace_target(
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
            let metadata_before =
                workspace::workspace_metadata(&config.state_dir, sandbox, workspace_selector)?;
            port_publisher.clear_workspace_ports(&metadata_before.sandbox_id, &metadata_before.id);
            let metadata =
                workspace::stop_workspace(&config.state_dir, sandbox, workspace_selector)?;
            Ok(serde_json::to_value(metadata)?)
        }
        "destroy" => {
            let metadata_before =
                workspace::workspace_metadata(&config.state_dir, sandbox, workspace_selector)?;
            port_publisher.clear_workspace_ports(&metadata_before.sandbox_id, &metadata_before.id);
            let removed =
                workspace::destroy_workspace(&config.state_dir, sandbox, workspace_selector)?;
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
            port_publisher.clear_workspace_ports(&metadata_before.sandbox_id, &metadata_before.id);
            workspace::remove_workspace(&config.state_dir, sandbox, workspace_selector)?;
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

fn dispatch_workspace_list(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let sandbox_id = params.get("sandbox_id").and_then(Value::as_str);
    if let Some(selector) = sandbox_id {
        let items = workspace::list_workspace_items(&config.state_dir, selector)?;
        return Ok(serde_json::to_value(items)?);
    }
    let workspaces = workspace::list_workspaces(&config.state_dir, None)?;
    Ok(serde_json::to_value(workspaces)?)
}

fn dispatch_workspace_update(
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

fn dispatch_workspace_exec(params: &Value, config: &DaemonConfig) -> Result<Value> {
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

fn dispatch_workspace_logs(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let tail = params
        .get("tail")
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    let result = workspace::workspace_logs(&config.state_dir, sandbox, workspace_selector, tail)?;
    Ok(serde_json::to_value(result)?)
}

fn dispatch_workspace_port_publish(
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

fn dispatch_workspace_port_unpublish(
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

fn dispatch_workspace_port_list(
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

fn dispatch_workspace_snapshot(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let name = params.get("name").and_then(Value::as_str);
    let result =
        workspace::create_workspace_snapshot(&config.state_dir, sandbox, workspace_selector, name)?;
    Ok(serde_json::to_value(result)?)
}

fn dispatch_workspace_restore(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let snapshot = require_param_str(params, &["snapshot"])?;
    let result = workspace::restore_workspace_snapshot(
        &config.state_dir,
        sandbox,
        workspace_selector,
        snapshot,
    )?;
    Ok(serde_json::to_value(result)?)
}

fn dispatch_workspace_snapshot_gc(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let keep = params
        .get("keep")
        .and_then(Value::as_u64)
        .unwrap_or(workspace::DEFAULT_SNAPSHOT_KEEP as u64) as usize;
    let removed =
        workspace::gc_workspace_snapshots(&config.state_dir, sandbox, workspace_selector, keep)?;
    Ok(serde_json::to_value(removed)?)
}

fn dispatch_policy_rule(params: &Value, config: &DaemonConfig, is_allow: bool) -> Result<Value> {
    let uid = params.get("uid").and_then(Value::as_u64).map(|v| v as u32);
    let action = require_param_str(params, &["action"])?;
    let updated = if is_allow {
        policy::add_allow_rule(&config.state_dir, uid, action)?
    } else {
        policy::add_deny_rule(&config.state_dir, uid, action)?
    };
    Ok(serde_json::to_value(updated)?)
}

#[derive(Debug, Clone, Copy)]
enum Action {
    Ping,
    DaemonHealth,
    DaemonDoctor,
    Init,
    SandboxCreate,
    SandboxUpdate,
    SandboxStart,
    SandboxStop,
    SandboxStatus,
    SandboxDestroy,
    SandboxList,
    SandboxRemove,
    SandboxExecSetup,
    ProcessList,
    WorkspaceCreate,
    WorkspaceStart,
    WorkspaceStop,
    WorkspaceDestroy,
    WorkspaceStatus,
    WorkspaceStats,
    WorkspaceStatsList,
    WorkspaceList,
    WorkspaceRemove,
    WorkspaceUpdate,
    WorkspaceExec,
    WorkspacePortPublish,
    WorkspacePortUnpublish,
    WorkspacePortList,
    WorkspaceRuntime,
    WorkspaceLogs,
    WorkspaceSnapshot,
    WorkspaceSnapshotList,
    WorkspaceRestore,
    WorkspaceSnapshotGc,
    RegistryRepair,
    PolicyGet,
    PolicySetDefault,
    PolicyAllow,
    PolicyDeny,
    PolicyClear,
    Shutdown,
}

impl Action {
    fn parse(raw: &str) -> Result<Self> {
        let action = match raw {
            "ping" => Self::Ping,
            "daemon.health" => Self::DaemonHealth,
            "daemon.doctor" => Self::DaemonDoctor,
            "init" => Self::Init,
            "sandbox.create" => Self::SandboxCreate,
            "sandbox.update" => Self::SandboxUpdate,
            "sandbox.start" => Self::SandboxStart,
            "sandbox.stop" => Self::SandboxStop,
            "sandbox.status" => Self::SandboxStatus,
            "sandbox.destroy" => Self::SandboxDestroy,
            "sandbox.list" => Self::SandboxList,
            "sandbox.remove" => Self::SandboxRemove,
            "sandbox.exec_setup" => Self::SandboxExecSetup,
            "process.list" => Self::ProcessList,
            "workspace.create" => Self::WorkspaceCreate,
            "workspace.start" => Self::WorkspaceStart,
            "workspace.stop" => Self::WorkspaceStop,
            "workspace.destroy" => Self::WorkspaceDestroy,
            "workspace.status" => Self::WorkspaceStatus,
            "workspace.stats" => Self::WorkspaceStats,
            "workspace.stats.list" => Self::WorkspaceStatsList,
            "workspace.list" => Self::WorkspaceList,
            "workspace.remove" => Self::WorkspaceRemove,
            "workspace.update" | "workspace.update_auth" => Self::WorkspaceUpdate,
            "workspace.exec" => Self::WorkspaceExec,
            "workspace.port.publish" => Self::WorkspacePortPublish,
            "workspace.port.unpublish" => Self::WorkspacePortUnpublish,
            "workspace.port.list" => Self::WorkspacePortList,
            "workspace.runtime" => Self::WorkspaceRuntime,
            "workspace.logs" => Self::WorkspaceLogs,
            "workspace.snapshot" => Self::WorkspaceSnapshot,
            "workspace.snapshot.list" => Self::WorkspaceSnapshotList,
            "workspace.restore" => Self::WorkspaceRestore,
            "workspace.snapshot.gc" => Self::WorkspaceSnapshotGc,
            "registry.repair" => Self::RegistryRepair,
            "policy.get" => Self::PolicyGet,
            "policy.set_default" => Self::PolicySetDefault,
            "policy.allow" => Self::PolicyAllow,
            "policy.deny" => Self::PolicyDeny,
            "policy.clear" => Self::PolicyClear,
            "shutdown" => Self::Shutdown,
            _ => bail!("unknown action '{}'", raw),
        };
        Ok(action)
    }
}

fn parse_published_ports(
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

fn ensure_workspace_ports_started(
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

fn update_workspace_definition_with_runtime(
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

    let apply_result = if updated.status == workspace::WorkspaceStatus::Running {
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

struct WorkspaceDefinitionUpdateRequest<'a> {
    sandbox: &'a str,
    workspace_selector: &'a str,
    auth_providers: Option<Vec<String>>,
    env_tokens: Option<Vec<String>>,
    published_ports: Option<Vec<workspace::PublishedPortSpec>>,
    limits_update: workspace::WorkspaceLimitsUpdate,
}

fn rollback_workspace_definition_update(
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

    if previous.status == workspace::WorkspaceStatus::Running {
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

fn workspace_port_statuses(
    metadata: &workspace::WorkspaceMetadata,
    port_publisher: &Arc<PortPublisher>,
) -> Vec<workspace::PublishedPortStatus> {
    workspace::merge_published_port_statuses(
        &metadata.published_ports,
        &port_publisher.workspace_statuses(&metadata.sandbox_id, &metadata.id),
    )
}

#[cfg(test)]
#[path = "../../tests/src/daemon/dispatch.rs"]
mod tests;
