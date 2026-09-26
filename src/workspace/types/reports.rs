//! What a workspace command reports back.
//!
//! Each of these is the response of one command, so the shapes are grouped by
//! the command they answer rather than by the fields they happen to share.

use serde::{Deserialize, Serialize};

use super::limits::WorkspaceLimits;
use super::status::WorkspaceStatus;
use crate::sandbox::SandboxLimits;
use crate::workspace::ports::PublishedPortStatus;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub mount_ns: String,
    pub pid_ns: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceResizeResult {
    pub workspace_id: String,
    pub workspace_name: String,
    pub previous_disk_bytes: u64,
    pub new_disk_bytes: u64,
    #[serde(default)]
    pub previous_memory_bytes: Option<u64>,
    #[serde(default)]
    pub new_memory_bytes: Option<u64>,
    pub restarted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceCpResult {
    pub workspace_id: String,
    pub workspace_name: String,
    pub logical_bytes: u64,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceListItem {
    pub id: String,
    pub name: String,
    pub status: WorkspaceStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceStatusReport {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub allocated_path: String,
    pub status: WorkspaceStatus,
    pub active_process_count: usize,
    pub resource_usage: Option<String>,
    #[serde(default)]
    pub limits: WorkspaceLimits,
    #[serde(default)]
    pub sandbox_limits: SandboxLimits,
    #[serde(default)]
    pub published_ports: Vec<PublishedPortStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceStatsReport {
    pub id: String,
    pub sandbox_id: String,
    pub name: String,
    pub status: WorkspaceStatus,
    pub pid: Option<u32>,
    pub active_process_count: usize,
    pub threads: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub cpu_limit_percent: Option<f64>,
    pub sandbox_cpu_limit_percent: Option<f64>,
    pub memory_usage_bytes: Option<u64>,
    pub memory_limit_bytes: Option<u64>,
    pub sandbox_memory_limit_bytes: Option<u64>,
    pub memory_percent: Option<f64>,
    pub net_rx_bytes: Option<u64>,
    pub net_tx_bytes: Option<u64>,
    pub block_read_bytes: Option<u64>,
    pub block_write_bytes: Option<u64>,
    pub pids_current: Option<u64>,
    pub pids_limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRuntimeInfo {
    pub sandbox_id: String,
    pub workspace_id: String,
    pub workspace_name: String,
    pub runtime_pid: u32,
    pub runtime_starttime_ticks: u64,
    pub sandbox_rootfs_path: String,
    /// Cgroup that holds the workspace's processes, when the host enforces
    /// limits with cgroup v2. Callers that spawn helpers into the workspace
    /// must attach them here so the workspace limits apply to them.
    #[serde(default)]
    pub cgroup_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceLogsResult {
    pub content: String,
    #[serde(default)]
    pub next_offset: u64,
    #[serde(default)]
    pub reset: bool,
    /// Identity of the log file these offsets refer to.
    ///
    /// A follower passes it back with its offset. If the file has been replaced
    /// since — the same path now names a different inode — the offsets no
    /// longer describe the content, so the daemon answers with a reset instead
    /// of a slice of unrelated bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_id: Option<String>,
    /// Whether more content is already waiting beyond this response.
    ///
    /// A follow poll returns a bounded chunk, so the follower needs to know
    /// whether to come back immediately or to wait for new output.
    #[serde(default)]
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSnapshotInfo {
    pub name: String,
    pub created_at: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSnapshotArchiveInfo {
    pub name: String,
    pub archive_path: String,
}
