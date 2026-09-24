use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::network::publish::PortPublisher;
use crate::policy;
use crate::registry;
use crate::sandbox::{self, BootstrapMethod, DEFAULT_DEBIAN_MIRROR, DEFAULT_DEBIAN_SUITE};
use crate::workspace;

use super::DaemonConfig;

mod ports;
mod sandbox_handlers;
mod snapshots;
mod workspace_handlers;

use ports::{
    dispatch_workspace_port_list, dispatch_workspace_port_publish,
    dispatch_workspace_port_unpublish,
};
use sandbox_handlers::{
    dispatch_sandbox_create, dispatch_sandbox_destroy, dispatch_sandbox_pause,
    dispatch_sandbox_resume, dispatch_sandbox_start, dispatch_sandbox_status,
    dispatch_sandbox_stop, dispatch_sandbox_update, dispatch_sandbox_wipe,
};
use snapshots::{
    dispatch_workspace_restore, dispatch_workspace_snapshot, dispatch_workspace_snapshot_export,
    dispatch_workspace_snapshot_gc, dispatch_workspace_snapshot_import,
};
#[cfg(test)]
use workspace_handlers::existing_workspace_update;
use workspace_handlers::{
    dispatch_workspace_cp, dispatch_workspace_create, dispatch_workspace_exec,
    dispatch_workspace_list, dispatch_workspace_logs, dispatch_workspace_resize,
    dispatch_workspace_start_many, dispatch_workspace_target, dispatch_workspace_update,
};

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
    let memory_bytes = checked_megabytes(memory_mb, "memory_mb")?;
    let disk_bytes = checked_megabytes(disk_mb, "disk_mb")?;
    let limits = workspace::WorkspaceLimits {
        cpu_seconds,
        cpu_percent,
        memory_bytes,
        max_processes: max_procs,
        max_open_files,
        disk_bytes,
    };
    limits.validate()?;
    Ok(limits)
}

fn parse_required_disk_bytes(params: &Value) -> Result<u64> {
    let disk_mb = params
        .get("disk_mb")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("missing 'disk_mb' unsigned integer"))?;
    disk_mb
        .checked_mul(1024 * 1024)
        .ok_or_else(|| anyhow::anyhow!("'disk_mb' is too large"))
}

fn parse_workspace_limits_update(params: &Value) -> Result<workspace::WorkspaceLimitsUpdate> {
    let memory_bytes =
        checked_optional_megabytes(parse_optional_u64_field(params, "memory_mb")?, "memory_mb")?;
    let disk_bytes =
        checked_optional_megabytes(parse_optional_u64_field(params, "disk_mb")?, "disk_mb")?;
    Ok(workspace::WorkspaceLimitsUpdate {
        clear_tmp_on_restart: parse_optional_bool_field(params, "clear_tmp_on_restart")?,
        cpu_seconds: parse_optional_u64_field(params, "cpu_seconds")?,
        cpu_percent: parse_optional_f64_field(params, "cpu_percent")?,
        memory_bytes,
        max_processes: parse_optional_u64_field(params, "max_procs")?,
        max_open_files: parse_optional_u64_field(params, "max_open_files")?,
        disk_bytes,
    })
}

fn checked_megabytes(value: Option<u64>, key: &str) -> Result<Option<u64>> {
    value
        .map(|value| {
            value
                .checked_mul(1024 * 1024)
                .ok_or_else(|| anyhow::anyhow!("'{key}' is too large"))
        })
        .transpose()
}

fn checked_optional_megabytes(
    value: Option<Option<u64>>,
    key: &str,
) -> Result<Option<Option<u64>>> {
    value.map(|value| checked_megabytes(value, key)).transpose()
}

fn parse_optional_bool_field(params: &Value, key: &str) -> Result<Option<bool>> {
    match params.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("'{key}' must be a boolean")),
    }
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
    client_stream: Option<&UnixStream>,
) -> Result<Value> {
    let action = Action::parse(&request.action)?;
    match action {
        Action::Ping => Ok(json!({"status": "pong"})),
        Action::DaemonHealth => Ok(json!({
            "status": "ok",
            "pid": std::process::id(),
            "state_dir": config.state_dir.to_string_lossy(),
            "socket_path": config.socket_path.to_string_lossy(),
            "metrics": crate::perf::metrics(),
        })),
        Action::DaemonDoctor => {
            let report = crate::doctor::run_doctor(&config.state_dir)?;
            Ok(serde_json::to_value(report)?)
        }
        Action::DaemonDoctorRepair => {
            let report = crate::doctor::repair_doctor(&config.state_dir, &config.socket_path)?;
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
        Action::SandboxPause => dispatch_sandbox_pause(&request.params, config, port_publisher),
        Action::SandboxResume => dispatch_sandbox_resume(&request.params, config, port_publisher),
        Action::SandboxStatus => dispatch_sandbox_status(&request.params, config),
        Action::SandboxDestroy => dispatch_sandbox_destroy(&request.params, config, port_publisher),
        Action::SandboxWipe => dispatch_sandbox_wipe(config, port_publisher),
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
            let cache_setup = request
                .params
                .get("cache_setup")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let setup_digest = request.params.get("setup_digest").and_then(Value::as_str);
            let setup_index = request.params.get("setup_index").and_then(Value::as_u64);
            sandbox::exec_setup_command(
                &config.state_dir,
                selector,
                command,
                cache_setup,
                setup_digest,
                setup_index,
            )
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
        Action::WorkspaceStartMany => {
            dispatch_workspace_start_many(&request.params, config, port_publisher)
        }
        Action::WorkspaceStop => {
            dispatch_workspace_target(&request.params, config, "stop", port_publisher)
        }
        Action::WorkspaceDestroy => {
            dispatch_workspace_target(&request.params, config, "destroy", port_publisher)
        }
        Action::WorkspaceWipe => {
            let workspaces = workspace::list_workspaces(&config.state_dir, None)?;
            let report = workspace::destroy_all_workspaces(&config.state_dir)?;
            for workspace in &workspaces {
                if report.removed.contains(&workspace.id) {
                    port_publisher.clear_workspace_ports(&workspace.sandbox_id, &workspace.id);
                }
            }
            if !report.errors.is_empty() {
                bail!(
                    "workspace wipe completed with {} error(s): {}",
                    report.errors.len(),
                    report.errors.join("; ")
                );
            }
            Ok(serde_json::to_value(report)?)
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
        Action::WorkspaceResize => {
            dispatch_workspace_resize(&request.params, config, port_publisher)
        }
        Action::WorkspaceExec => dispatch_workspace_exec(&request.params, config),
        Action::WorkspaceCp => dispatch_workspace_cp(&request.params, config, client_stream),
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
        Action::WorkspaceSnapshotExport => {
            dispatch_workspace_snapshot_export(&request.params, config)
        }
        Action::WorkspaceSnapshotImport => {
            dispatch_workspace_snapshot_import(&request.params, config)
        }
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

pub(super) fn dispatch_policy_rule(
    params: &Value,
    config: &DaemonConfig,
    is_allow: bool,
) -> Result<Value> {
    let uid = params.get("uid").and_then(Value::as_u64).map(|v| v as u32);
    let action = require_param_str(params, &["action"])?;
    let updated = if is_allow {
        policy::add_allow_rule(&config.state_dir, uid, action)?
    } else {
        policy::add_deny_rule(&config.state_dir, uid, action)?
    };
    Ok(serde_json::to_value(updated)?)
}

mod action;
use action::Action;
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
#[path = "../../../tests/src/daemon/dispatch.rs"]
mod tests;
