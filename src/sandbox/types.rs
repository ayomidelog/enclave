use std::fmt;
use std::str::FromStr;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::resource_limits::validate_cpu_percent;

pub const DEFAULT_DEBIAN_SUITE: &str = "bookworm";
pub const DEFAULT_DEBIAN_MIRROR: &str = "http://deb.debian.org/debian";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapMethod {
    #[default]
    Debootstrap,
    CachedRootfs,
}

impl fmt::Display for BootstrapMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BootstrapMethod::Debootstrap => write!(f, "debootstrap"),
            BootstrapMethod::CachedRootfs => write!(f, "cached_rootfs"),
        }
    }
}

impl FromStr for BootstrapMethod {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "debootstrap" => Ok(BootstrapMethod::Debootstrap),
            "cached_rootfs" => Ok(BootstrapMethod::CachedRootfs),
            _ => bail!(
                "unknown bootstrap method '{}'; valid values: debootstrap, cached_rootfs",
                s
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SandboxStatus {
    Running,
    #[default]
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SandboxLimits {
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<u64>,
    pub max_processes: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct SandboxLimitsUpdate {
    pub cpu_percent: Option<Option<f64>>,
    pub memory_bytes: Option<Option<u64>>,
    pub max_processes: Option<Option<u64>>,
}

impl SandboxLimits {
    pub fn validate(&self) -> Result<()> {
        if let Some(cpu_percent) = self.cpu_percent {
            validate_cpu_percent(cpu_percent)?;
        }
        Ok(())
    }

    pub fn has_limits(&self) -> bool {
        self.cpu_percent.is_some() || self.memory_bytes.is_some() || self.max_processes.is_some()
    }

    pub fn apply_update(&mut self, update: &SandboxLimitsUpdate) -> Result<bool> {
        let mut changed = false;
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
        self.validate()?;
        Ok(changed)
    }
}

impl SandboxLimitsUpdate {
    pub fn is_empty(&self) -> bool {
        self.cpu_percent.is_none() && self.memory_bytes.is_none() && self.max_processes.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxMetadata {
    pub id: String,
    pub name: String,
    pub suite: String,
    pub mirror: String,
    #[serde(default)]
    pub bootstrap_method: BootstrapMethod,
    pub created_at: String,
    pub sandbox_path: String,
    pub rootfs_path: String,
    #[serde(default)]
    pub mounted_rootfs_path: String,
    #[serde(default)]
    pub workspaces_path: String,
    #[serde(default)]
    pub home_base_path: String,
    #[serde(default)]
    pub limits: SandboxLimits,
    #[serde(default)]
    pub status: SandboxStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxListItem {
    pub id: String,
    pub name: String,
    pub status: SandboxStatus,
    pub workspace_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxStatusReport {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub status: SandboxStatus,
    pub rootfs_path: String,
    pub rootfs_disk_usage_bytes: u64,
    pub workspace_count: usize,
    #[serde(default)]
    pub limits: SandboxLimits,
}
