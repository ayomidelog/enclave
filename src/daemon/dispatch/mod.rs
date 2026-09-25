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

mod action;
mod lease_scope;
mod params;
mod ports;
mod sandbox_handlers;
mod snapshots;
mod transition;
mod workspace_definition;
mod workspace_handlers;

use action::Action;

/// Whether `action` names a lifecycle operation.
///
/// The caller needs this before dispatch, for the request log line, so the
/// classification is exposed here rather than reached through `Action` directly.
pub(super) fn action_is_lifecycle(action: &str) -> bool {
    Action::parse(action).is_ok_and(Action::is_lifecycle)
}
use params::{
    parse_cleanup_mode, parse_optional_bool_field, parse_required_disk_bytes,
    parse_sandbox_limits_create, parse_sandbox_limits_update, parse_string_array,
    parse_workspace_limits_create, parse_workspace_limits_update, require_param_str,
};
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
use transition::{sandbox_state_before, with_transition, workspace_state_before, ABSENT};
use workspace_definition::{
    ensure_workspace_ports_started, parse_published_ports,
    update_workspace_definition_with_runtime, workspace_port_statuses,
    WorkspaceDefinitionUpdateRequest,
};
#[cfg(test)]
use workspace_handlers::existing_workspace_update;
use workspace_handlers::{
    dispatch_workspace_cp, dispatch_workspace_create, dispatch_workspace_exec,
    dispatch_workspace_list, dispatch_workspace_logs, dispatch_workspace_resize,
    dispatch_workspace_start_many, dispatch_workspace_target, dispatch_workspace_update,
};

pub(crate) fn dispatch(
    request: crate::protocol::Request,
    config: &DaemonConfig,
    shutdown: &Arc<AtomicBool>,
    services: &crate::daemon::services::DaemonServices,
    client_stream: Option<&UnixStream>,
) -> Result<Value> {
    let action = Action::parse(&request.action)?;
    let port_publisher = &services.port_publisher;
    // Serialize the requests that touch the same resources. Read-only and
    // per-command requests take no lease, so they keep answering while a
    // lifecycle operation runs.
    let _lease = match lease_scope::scope_for(&config.state_dir, action, &request.params)? {
        Some(scope) => Some(services.leases.acquire(scope)?),
        None => None,
    };
    match action {
        Action::Ping => Ok(json!({"status": "pong"})),
        Action::DaemonHealth => Ok(json!({
            "status": "ok",
            "pid": std::process::id(),
            "state_dir": config.state_dir.to_string_lossy(),
            "socket_path": config.socket_path.to_string_lossy(),
            // Lifecycle operations running right now, so a slow daemon can be
            // explained without reading the log.
            "active_operations": services
                .active_operations
                .in_flight()
                .iter()
                .map(crate::daemon::active_operations::ActiveOperation::describe)
                .collect::<Vec<_>>(),
            // The last lifecycle operation the daemon ran, so a status report can
            // name it without the operator reading the journal directory.
            "last_operation": crate::operation::latest(&config.state_dir)?,
            // The deadlines actually in force, so a wait that ended early or late
            // can be explained without reading the environment of the daemon.
            "deadlines": crate::deadlines::describe_all(),
            "metrics": crate::perf::metrics(),
        })),
        Action::DaemonDoctor => {
            // The daemon owns the port publisher and the running operations, so
            // this is the one caller that can report a published listener no
            // running workspace is using and tell a live operation apart from one
            // whose daemon died.
            let daemon = crate::doctor::DaemonState {
                port_publisher,
                active_operations: services.active_operations.as_ref(),
            };
            let report = crate::doctor::run_doctor(&config.state_dir, Some(&daemon))?;
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
        Action::SandboxWipe => dispatch_sandbox_wipe(&request.params, config, port_publisher),
        Action::SandboxList => {
            let sandboxes = sandbox::list_sandbox_items(&config.state_dir)?;
            Ok(serde_json::to_value(sandboxes)?)
        }
        Action::SandboxRemove => {
            let selector = require_param_str(&request.params, &["sandbox", "sandbox_id"])?;
            let previous_state = sandbox_state_before(&config.state_dir, selector);
            let removed = sandbox::destroy_sandbox(&config.state_dir, selector)?;
            Ok(with_transition(
                json!({ "removed": removed }),
                previous_state,
                ABSENT,
            ))
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
            let setup_commands = request
                .params
                .get("setup_commands")
                .and_then(Value::as_array)
                .map(|commands| {
                    commands
                        .iter()
                        .map(|command| {
                            command.as_str().map(str::to_string).ok_or_else(|| {
                                anyhow::anyhow!("setup_commands entries must be strings")
                            })
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?;
            let setup_index = request.params.get("setup_index").and_then(Value::as_u64);
            sandbox::exec_setup_command(
                &config.state_dir,
                selector,
                command,
                cache_setup,
                setup_commands.as_deref(),
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
            let mode = parse_cleanup_mode(&request.params)?;
            let workspaces = workspace::list_workspaces(&config.state_dir, None)?;
            let report = workspace::destroy_all_workspaces(&config.state_dir, mode)?;
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
            let mut report = registry::repair_registry(&config.state_dir, strict)?;
            // Repair fixes registry/disk consistency; this brings the persisted
            // lifecycle state back in line with what is actually running, so an
            // interrupted start or stop is recoverable without a daemon restart.
            report.reconciled_runtime_records =
                crate::sandbox::reconcile_runtime_state(&config.state_dir)?;
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
            super::shutdown::SIGNAL_SHUTDOWN.store(true, Ordering::SeqCst);
            // This runs on a worker thread, so no signal is delivered to the accept
            // loop; the pipe is what tells it to stop waiting.
            super::shutdown::wake_shutdown_wait();
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
#[cfg(test)]
#[path = "../../../tests/src/daemon/dispatch.rs"]
mod tests;
