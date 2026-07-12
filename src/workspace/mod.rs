mod control;
mod create;
mod cwd;
mod exec;
mod logs;
mod ports;
pub mod ps;
mod runtime;
pub(crate) mod session;
mod snapshot;
mod stats;
mod storage;
mod types;

pub const DEFAULT_WORKSPACE_PATH: &str =
    "/opt/flutter/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

pub use crate::network::publish::PortPublisher;
pub(crate) use control::start_workspace_with_security;
pub use control::{
    destroy_workspace, list_workspace_items, list_workspaces, remove_workspace, start_workspace,
    stop_workspace, workspace_status,
};
pub(crate) use control::{
    stop_running_workspaces_in_sandbox, sync_sandbox_runtime_limits,
    sync_workspace_runtime_limits, update_workspace_definition, workspace_metadata,
};
pub use create::{create_workspace, create_workspace_with_options, WorkspaceCreateOptions};
pub use exec::exec_workspace_command;
pub use logs::workspace_logs;
pub use ports::{
    configured_port_statuses, merge_published_port_statuses, validate_published_ports,
    PublishedPortBinding, PublishedPortSpec, PublishedPortState, PublishedPortStatus,
};
pub use ps::{list_process_status, ProcessEntry};
pub use runtime::workspace_runtime_info;
pub use snapshot::{
    create_workspace_snapshot, gc_workspace_snapshots, list_workspace_snapshots,
    restore_workspace_snapshot, DEFAULT_SNAPSHOT_KEEP,
};
pub use stats::{list_running_workspace_stats, workspace_stats};
pub use types::{
    WorkspaceExecResult, WorkspaceLimits, WorkspaceLimitsUpdate, WorkspaceListItem,
    WorkspaceLogsResult, WorkspaceMetadata, WorkspaceRuntimeInfo, WorkspaceSnapshotInfo,
    WorkspaceStatsReport, WorkspaceStatus, WorkspaceStatusReport,
};

pub fn session_process_matches(pid: u32, expected_starttime_ticks: Option<u64>) -> bool {
    session::process_matches(pid, expected_starttime_ticks)
}

pub(crate) use cwd::sanitize_workspace_cwd;
pub(crate) use storage::{
    create_workspace_storage, ensure_workspace_storage_ready, ensure_workspace_storage_unmounted,
    validate_workspace_storage_limits, with_workspace_storage_mounted,
};
