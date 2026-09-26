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
    pub clear_tmp_on_restart: bool,
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
