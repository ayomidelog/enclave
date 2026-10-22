//! The record of one workspace, and the namespace references it holds.

use serde::{Deserialize, Serialize};

use super::limits::WorkspaceLimits;
use super::status::WorkspaceStatus;
use crate::workspace::ports::PublishedPortSpec;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamespaceRefs {
    pub mount: String,
    pub pid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceMetadata {
    pub id: String,
    pub sandbox_id: String,
    pub name: String,
    pub created_at: String,
    /// The last real request that acted on this workspace.
    ///
    /// Status and stats reads deliberately do not update it. The idle sweeper
    /// uses it to tell a workspace somebody is working in from one that is only
    /// still running because nobody stopped it.
    #[serde(default)]
    pub last_request_at: Option<String>,
    /// The last time a command or detached job made observable progress.
    #[serde(default)]
    pub last_process_activity_at: Option<String>,
    /// When the runtime was last stopped. This survives the stop so the separate
    /// destroy-after policy can measure a stopped workspace's age.
    #[serde(default)]
    pub last_stopped_at: Option<String>,
    pub workspace_path: String,
    pub filesystem_path: String,
    pub filesystem_mount_target: String,
    #[serde(default)]
    pub home_mount_source_path: Option<String>,
    pub sandbox_rootfs_path: String,
    pub overlay_home_base_path: String,
    pub overlay_home_upper_path: String,
    pub overlay_home_work_path: String,
    pub overlay_home_merged_path: String,
    #[serde(default)]
    pub auth_providers: Vec<String>,
    /// The auth namespace this workspace's tokens are read from.
    ///
    /// Absent means the state directory's shared namespace, which is what every
    /// workspace used before user namespaces existed, so an owner-less
    /// workspace resolves exactly as it always did.
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub env_tokens: Vec<String>,
    #[serde(default)]
    pub published_ports: Vec<PublishedPortSpec>,
    #[serde(default)]
    pub status: WorkspaceStatus,
    #[serde(default)]
    pub runtime_pid: Option<u32>,
    #[serde(default)]
    pub runtime_starttime_ticks: Option<u64>,
    #[serde(default)]
    pub namespace_refs: NamespaceRefs,
    #[serde(default)]
    pub clear_tmp_on_restart: bool,
    /// Exempt this workspace from idle stops and destroy-after retention deletion.
    #[serde(default)]
    pub keep_running: bool,
    /// Stop an idle runtime after this many seconds. `None` inherits the daemon
    /// default and zero disables the idle stop for this workspace.
    #[serde(default)]
    pub idle_stop_after_secs: Option<u64>,
    /// Destroy a stopped workspace after this many seconds. `None` inherits the
    /// daemon default and zero keeps its files indefinitely.
    #[serde(default)]
    pub destroy_after_secs: Option<u64>,
    #[serde(default)]
    pub limits: WorkspaceLimits,

    #[serde(default)]
    pub assigned_ip: Option<String>,
}

impl Default for NamespaceRefs {
    fn default() -> Self {
        Self {
            mount: "unassigned".to_string(),
            pid: "unassigned".to_string(),
        }
    }
}
