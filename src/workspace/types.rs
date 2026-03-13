use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::resource_limits::validate_cpu_percent;
use crate::sandbox::SandboxLimits;

use super::ports::PublishedPortSpec;
use super::ports::PublishedPortStatus;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceStatus {
    Running,
    #[default]
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamespaceRefs {
    pub mount: String,
    pub pid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceLimits {
    pub cpu_seconds: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<u64>,
    pub max_processes: Option<u64>,
    pub max_open_files: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceLimitsUpdate {
    pub cpu_seconds: Option<Option<u64>>,
    pub cpu_percent: Option<Option<f64>>,
    pub memory_bytes: Option<Option<u64>>,
    pub max_processes: Option<Option<u64>>,
    pub max_open_files: Option<Option<u64>>,
}

impl WorkspaceLimits {
    pub fn validate(&self) -> Result<()> {
        if let Some(cpu_percent) = self.cpu_percent {
            validate_cpu_percent(cpu_percent)?;
        }
        Ok(())
    }

    pub fn cgroup_limits_present(&self) -> bool {
        self.cpu_percent.is_some() || self.memory_bytes.is_some() || self.max_processes.is_some()
    }

    pub fn cpu_percent_requires_cgroup(&self) -> bool {
        self.cpu_percent.is_some()
    }

    pub fn apply_update(&mut self, update: &WorkspaceLimitsUpdate) -> Result<bool> {
        let mut changed = false;
        if let Some(cpu_seconds) = update.cpu_seconds {
            changed |= self.cpu_seconds != cpu_seconds;
            self.cpu_seconds = cpu_seconds;
        }
        if let Some(cpu_percent) = update.cpu_percent {
            if let Some(value) = cpu_percent {
                validate_cpu_percent(value)?;
            }
            changed |= self.cpu_percent != cpu_percent;
            self.cpu_percent = cpu_percent;
        }
        if let Some(memory_bytes) = update.memory_bytes {
            changed |= self.memory_bytes != memory_bytes;
            self.memory_bytes = memory_bytes;
        }
        if let Some(max_processes) = update.max_processes {
            changed |= self.max_processes != max_processes;
            self.max_processes = max_processes;
        }
        if let Some(max_open_files) = update.max_open_files {
            changed |= self.max_open_files != max_open_files;
            self.max_open_files = max_open_files;
        }
        self.validate()?;
        Ok(changed)
    }
}

impl WorkspaceLimitsUpdate {
    pub fn is_empty(&self) -> bool {
        self.cpu_seconds.is_none()
            && self.cpu_percent.is_none()
            && self.memory_bytes.is_none()
            && self.max_processes.is_none()
            && self.max_open_files.is_none()
    }
}

impl Default for NamespaceRefs {
    fn default() -> Self {
        Self {
            mount: "unassigned".to_string(),
            pid: "unassigned".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceMetadata {
    pub id: String,
    pub sandbox_id: String,
    pub name: String,
    pub created_at: String,
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
    pub limits: WorkspaceLimits,

    #[serde(default)]
    pub assigned_ip: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub mount_ns: String,
    pub pid_ns: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceLogsResult {
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSnapshotInfo {
    pub name: String,
    pub created_at: String,
    pub path: String,
}
