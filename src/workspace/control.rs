//! Workspace lifecycle orchestration.
//!
//! Each lifecycle concern lives in its own module: `query` for read-only
//! lookups, `start` and `stop` for runtime control, `destroy` for removal,
//! `update` for definition and size changes, and `state` for the registry
//! transitions they share.

use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, bail, Context, Result};

use crate::network;
use crate::registry::{with_registry, with_registry_mut, RegistrySandbox};
use crate::sandbox::{effective_rootfs_path, resolve_sandbox_id, SandboxMetadata, SandboxStatus};

use crate::workspace::cleanup::{self, CleanupMode, WorkspaceStopCleanup};
use crate::workspace::ports::{
    merge_published_port_statuses, PublishedPortSpec, PublishedPortStatus,
};
use crate::workspace::runtime_limits;
use crate::workspace::session;
use crate::workspace::types::{
    WorkspaceLimitsUpdate, WorkspaceListItem, WorkspaceMetadata, WorkspaceResizeResult,
    WorkspaceStatus, WorkspaceStatusReport,
};

mod destroy;
mod query;
mod start;
mod state;
mod stop;
mod update;

pub use destroy::{
    destroy_all_workspaces, destroy_workspace, destroy_workspace_with_mode, remove_workspace,
    BatchDestroyReport, WorkspaceDestroyReport,
};
pub use query::{
    list_workspace_items, list_workspaces, workspace_metadata, workspace_runtime_is_active,
    workspace_status,
};
pub(crate) use start::launch_workspace_runtime;
pub use start::{start_workspace, start_workspace_with_security};
pub(crate) use stop::{freeze_workspaces_in_sandbox, stop_running_workspaces_in_sandbox};
pub use stop::{stop_workspace, stop_workspace_with_certificate};
pub use update::{
    resize_workspace_disk, resize_workspace_disk_with_security, update_workspace_definition,
};

// Helpers shared between the lifecycle modules above.
pub(crate) use query::collect_all_used_ip_octets;
pub(crate) use state::{
    mark_workspace_stopped, normalize_namespace_ref_paths, persist_workspace_metadata,
    reconcile_workspace_runtime_state, remove_sandbox_cgroup, remove_sandbox_cgroup_if_idle,
    resolve_workspace_id, set_workspace_stopped,
};

/// Result of bringing a workspace runtime up, before it is recorded.
pub(crate) struct WorkspaceRuntimeStart {
    pid: u32,
    starttime_ticks: u64,
    mount_ns: String,
    pid_ns: String,
    assigned_ip: String,
}

/// How a starting workspace should obtain its network identity.
pub(crate) enum NetworkStartPlan {
    /// Allocate the first free address from a set the caller read while holding
    /// the registry lock. Only safe when the lock is held across the launch.
    AllocateFromUsedIps(BTreeSet<u8>),
    /// Use an address the caller already reserved in the registry.
    UseReserved(String),
}

#[cfg(test)]
#[path = "../../tests/src/workspace/control.rs"]
mod tests;
