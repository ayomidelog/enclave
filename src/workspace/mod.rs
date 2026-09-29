mod certificate;
mod cleanup;
mod control;
mod cp;
mod create;
mod cwd;
mod exec;
mod inventory;
mod logs;
mod orphans;
mod ports;
pub mod ps;
mod runtime;
mod runtime_limits;
pub(crate) mod session;
mod snapshot;
mod stats;
mod storage;
mod types;

pub const DEFAULT_WORKSPACE_PATH: &str =
    "/opt/flutter/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

pub use crate::network::publish::PortPublisher;
pub(crate) use certificate::{
    verify_workspace_cleanup, verify_workspace_destroyed, WorkspaceCleanupCertificate,
};
pub use cleanup::{CleanupMode, RetainedResource};
pub use control::resize_workspace_disk;
pub use control::resize_workspace_memory;
pub(crate) use control::start_workspace_with_security;
pub use control::stop_workspace_with_certificate;
pub use control::{
    destroy_all_workspaces, destroy_workspace, destroy_workspace_with_mode, list_workspace_items,
    list_workspaces, remove_workspace, start_workspace, stop_workspace, workspace_status,
    BatchDestroyReport, WorkspaceDestroyReport,
};
pub(crate) use control::{
    freeze_workspaces_in_sandbox, reconcile_workspace_runtime_state,
    resize_workspace_with_security, stop_running_workspaces_in_sandbox,
    update_workspace_definition, workspace_metadata, workspace_runtime_is_active,
    WorkspaceDefinitionUpdate,
};
pub use cp::copy_workspace_path;
pub(crate) use cp::copy_workspace_path_with_connection;
pub(crate) use cp::CopyOptions;
pub(crate) use create::sandbox_workspace_disk_bytes;
pub use create::{create_workspace, create_workspace_with_options, WorkspaceCreateOptions};
pub use exec::exec_workspace_command;
pub(crate) use exec::spawn_workspace_command_detached;
pub use inventory::{ResourceIdentity, ResourceInventory, ResourceKind};
pub use logs::workspace_logs;
pub use orphans::{find_orphan_runtime, OrphanRuntime};
pub use ports::{
    configured_port_statuses, merge_published_port_statuses, validate_published_ports,
    PublishedPortBinding, PublishedPortSpec, PublishedPortState, PublishedPortStatus,
};
pub use ps::{list_process_status, ProcessEntry};
pub use runtime::workspace_runtime_info;
pub(crate) use runtime_limits::{
    existing_workspace_cgroup_path, sync_sandbox_runtime_limits, sync_workspace_runtime_limits,
    workspace_cgroup_path,
};
pub use snapshot::{
    create_workspace_snapshot, export_workspace_snapshot_archive, gc_workspace_snapshots,
    import_workspace_snapshot_archive, list_workspace_snapshots, restore_workspace_snapshot,
    DEFAULT_SNAPSHOT_KEEP,
};
pub use stats::{list_running_workspace_stats, workspace_stats};
pub use types::{
    WorkspaceCpResult, WorkspaceExecResult, WorkspaceLimits, WorkspaceLimitsUpdate,
    WorkspaceListItem, WorkspaceLogsResult, WorkspaceMetadata, WorkspaceResizeResult,
    WorkspaceRuntimeInfo, WorkspaceSnapshotArchiveInfo, WorkspaceSnapshotInfo,
    WorkspaceStatsReport, WorkspaceStatus, WorkspaceStatusReport,
};

pub fn session_process_matches(pid: u32, expected_starttime_ticks: Option<u64>) -> bool {
    session::process_matches(pid, expected_starttime_ticks)
}

/// The session module's path and process helpers, for the inventory and its tests.
#[cfg(test)]
pub(crate) use session as session_for_tests;

pub(crate) use cwd::sanitize_workspace_cwd;
pub(crate) use storage::disk_backend_available;
#[cfg(test)]
pub(crate) use storage::LEGACY_WORKSPACE_TMP_DIR;
pub(crate) use storage::{
    create_workspace_storage, ensure_workspace_storage_ready, ensure_workspace_storage_unmounted,
    ensure_workspace_storage_unmounted_many, reset_workspace_tmp,
    unmount_mounts_at_or_below_excluding, validate_workspace_storage_limits,
    verify_workspace_source, with_workspace_storage_mounted, workspace_disk_image_path,
    workspace_tmp_path, workspace_uses_disk_image, WORKSPACE_TMP_DIR,
};
