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
mod daemon_handlers;
mod lease_scope;
mod params;
mod policy_handlers;
mod ports;
mod sandbox_exec_setup;
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
use daemon_handlers::{
    dispatch_daemon_doctor, dispatch_daemon_doctor_repair, dispatch_daemon_health, dispatch_init,
    dispatch_shutdown,
};
use params::{
    parse_cleanup_mode, parse_optional_bool_field, parse_required_disk_bytes,
    parse_sandbox_limits_create, parse_sandbox_limits_update, parse_string_array,
    parse_workspace_limits_create, parse_workspace_limits_update, require_param_str,
};
use policy_handlers::{
    dispatch_policy_clear, dispatch_policy_get, dispatch_policy_rule, dispatch_policy_set_default,
    dispatch_registry_repair,
};
use ports::{
    dispatch_workspace_port_list, dispatch_workspace_port_publish,
    dispatch_workspace_port_unpublish,
};
use sandbox_exec_setup::dispatch_sandbox_exec_setup;
use sandbox_handlers::{
    dispatch_sandbox_create, dispatch_sandbox_destroy, dispatch_sandbox_list,
    dispatch_sandbox_pause, dispatch_sandbox_remove, dispatch_sandbox_resume,
    dispatch_sandbox_start, dispatch_sandbox_status, dispatch_sandbox_stop,
    dispatch_sandbox_update, dispatch_sandbox_wipe,
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
    dispatch_workspace_wipe,
};

/// Route one request to the handler for its action.
///
/// The handlers live in the module for the area a request acts on, so this is only
/// the routing table: the action, the lease it needs, and the one call that
/// answers it.
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
        Action::DaemonHealth => dispatch_daemon_health(config, services),
        Action::DaemonDoctor => dispatch_daemon_doctor(config, port_publisher, services),
        Action::DaemonDoctorRepair => dispatch_daemon_doctor_repair(config),
        Action::Init => dispatch_init(config),
        Action::SandboxCreate => dispatch_sandbox_create(&request.params, config),
        Action::SandboxUpdate => dispatch_sandbox_update(&request.params, config),
        Action::SandboxStart => dispatch_sandbox_start(&request.params, config),
        Action::SandboxStop => dispatch_sandbox_stop(&request.params, config, port_publisher),
        Action::SandboxPause => dispatch_sandbox_pause(&request.params, config, port_publisher),
        Action::SandboxResume => dispatch_sandbox_resume(&request.params, config, port_publisher),
        Action::SandboxStatus => dispatch_sandbox_status(&request.params, config),
        Action::SandboxDestroy => dispatch_sandbox_destroy(&request.params, config, port_publisher),
        Action::SandboxWipe => dispatch_sandbox_wipe(&request.params, config, port_publisher),
        Action::SandboxList => dispatch_sandbox_list(config),
        Action::SandboxRemove => dispatch_sandbox_remove(&request.params, config),
        Action::SandboxExecSetup => dispatch_sandbox_exec_setup(&request.params, config),
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
        Action::WorkspaceWipe => dispatch_workspace_wipe(&request.params, config, port_publisher),
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
        Action::RegistryRepair => dispatch_registry_repair(&request.params, config),
        Action::PolicyGet => dispatch_policy_get(config),
        Action::PolicySetDefault => dispatch_policy_set_default(&request.params, config),
        Action::PolicyAllow => dispatch_policy_rule(&request.params, config, true),
        Action::PolicyDeny => dispatch_policy_rule(&request.params, config, false),
        Action::PolicyClear => dispatch_policy_clear(&request.params, config),
        Action::Shutdown => dispatch_shutdown(shutdown),
    }
}

#[cfg(test)]
#[path = "../../../tests/src/daemon/dispatch.rs"]
mod tests;
