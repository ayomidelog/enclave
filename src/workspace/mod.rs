mod activity;
pub mod activity_service;
mod cancel;
mod certificate;

#[cfg(test)]
#[path = "../../tests/src/workspace/cancel.rs"]
mod cancel_tests;
mod cleanup;
mod control;
mod cp;
mod create;
mod cwd;
mod exec;
mod inventory;
pub mod jobs;
mod logs;
mod orphans;
mod ports;
mod processes;
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

/// Process-wide defaults configured by the daemon before it serves requests.
static DEFAULT_WORKSPACE_CPU_PERCENT: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();
static DEFAULT_WORKSPACE_MAX_PROCESSES: std::sync::OnceLock<Option<u64>> =
    std::sync::OnceLock::new();
static MAX_RUNNING_WORKSPACES: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();

pub(crate) fn configure_default_limits(cpu_percent: Option<f64>, max_processes: Option<u64>) {
    let _ = DEFAULT_WORKSPACE_CPU_PERCENT.set(cpu_percent);
    let _ = DEFAULT_WORKSPACE_MAX_PROCESSES.set(max_processes);
}

pub(crate) fn default_workspace_cpu_percent() -> Option<f64> {
    DEFAULT_WORKSPACE_CPU_PERCENT.get().copied().flatten()
}

pub(crate) fn default_workspace_max_processes() -> Option<u64> {
    DEFAULT_WORKSPACE_MAX_PROCESSES.get().copied().flatten()
}

pub(crate) fn configure_max_running_workspaces(limit: Option<usize>) {
    let _ = MAX_RUNNING_WORKSPACES.set(limit);
}

pub(crate) fn daemon_max_running_workspaces() -> Option<usize> {
    MAX_RUNNING_WORKSPACES.get().copied().flatten()
}

pub use crate::network::publish::PortPublisher;
pub(crate) use cancel::cancel_file_path;
pub use cancel::{cancel_workspace_command, CancelOutcome};
pub(crate) use certificate::{
    verify_workspace_cleanup, verify_workspace_destroyed, WorkspaceCleanupCertificate,
};
pub(crate) use cleanup::retained_summary;
pub use cleanup::{CleanupMode, RetainedResource};

/// Resolve a sandbox and workspace selector to the workspace's host directory.
pub(crate) fn resolve_workspace_path(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> anyhow::Result<std::path::PathBuf> {
    use anyhow::Context as _;
    let workspace = crate::registry::with_registry(state_dir, |registry| {
        let sandbox_id = crate::sandbox::resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .with_context(|| format!("sandbox '{sandbox_id}' not found"))?;
        let workspace_id = control::resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get(&workspace_id)
            .with_context(|| format!("workspace '{workspace_id}' not found"))?;
        Ok(std::path::PathBuf::from(&workspace.workspace_path))
    })?;
    Ok(workspace)
}
pub use activity::{
    destroy_threshold, idle_for_secs, idle_threshold, is_idle, last_activity, mark_stopped,
    now as activity_now, parse_timestamp, touch as touch_activity,
    touch_registry_only as touch_activity_registry_only, ActivityKind,
};
pub use activity_service::{ActivityService, CommandLease};
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
pub(crate) use exec::spawn_workspace_command_detached;
pub use exec::{exec_workspace_command, exec_workspace_command_with_options, WorkspaceExecOptions};
pub use inventory::{ResourceIdentity, ResourceInventory, ResourceKind};
pub use jobs::{JobMode, JobRecord, JobStatus};
pub use logs::workspace_logs;
pub use orphans::{find_orphan_runtime, OrphanRuntime};
pub use ports::{
    configured_port_statuses, merge_published_port_statuses, validate_published_ports,
    PublishedPortBinding, PublishedPortSpec, PublishedPortState, PublishedPortStatus,
};
pub(crate) use processes::process_group_has_members;
pub use processes::{
    detached_count, job_process_count, list_workspace_processes, WorkspaceProcess,
};
pub use ps::{list_process_status, ProcessEntry};
pub use runtime::workspace_runtime_info;
pub(crate) use runtime_limits::{
    existing_workspace_cgroup_path, sync_sandbox_runtime_limits, sync_workspace_runtime_limits,
    workspace_cgroup_path,
};
pub(crate) use snapshot::temporary_workspace;
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
