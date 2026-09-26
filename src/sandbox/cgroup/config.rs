//! Turning a workspace's declared limits into the values a cgroup expects.

use anyhow::{Context, Result};

use crate::resource_limits::{cpu_quota_from_machine_percent, DEFAULT_CPU_CGROUP_PERIOD_US};

use super::layout::is_cgroup_v2_available;
use super::write_cgroup_value;

#[derive(Debug, Clone, Default)]
pub struct CgroupConfig {
    pub memory_bytes: Option<u64>,
    pub cpu_quota_us: Option<u64>,
    pub cpu_period_us: u64,
    pub pids_max: Option<u64>,
}

impl CgroupConfig {
    pub fn from_limits(
        memory_bytes: Option<u64>,
        cpu_percent: Option<f64>,
        max_processes: Option<u64>,
    ) -> Result<Self> {
        let cpu_period_us = DEFAULT_CPU_CGROUP_PERIOD_US;
        let cpu_quota_us = cpu_percent
            .map(|value| cpu_quota_from_machine_percent(value, cpu_period_us))
            .transpose()?;
        Ok(Self {
            memory_bytes,
            cpu_quota_us,
            cpu_period_us,
            pids_max: max_processes,
        })
    }

    pub fn has_limits(&self) -> bool {
        self.memory_bytes.is_some() || self.cpu_quota_us.is_some() || self.pids_max.is_some()
    }
}

pub(super) fn apply_cgroup_limits(
    cgroup_path: &std::path::Path,
    config: &CgroupConfig,
) -> Result<()> {
    if !is_cgroup_v2_available() {
        return Ok(());
    }

    write_cgroup_value(
        &cgroup_path.join("memory.max"),
        &limit_or_max(config.memory_bytes),
    )
    .with_context(|| format!("failed to set memory.max for {}", cgroup_path.display()))?;

    let cpu_value = match config.cpu_quota_us {
        Some(quota) => format!("{} {}", quota, config.cpu_period_us),
        None => format!("max {}", config.cpu_period_us),
    };
    write_cgroup_value(&cgroup_path.join("cpu.max"), &cpu_value)
        .with_context(|| format!("failed to set cpu.max for {}", cgroup_path.display()))?;

    write_cgroup_value(
        &cgroup_path.join("pids.max"),
        &limit_or_max(config.pids_max),
    )
    .with_context(|| format!("failed to set pids.max for {}", cgroup_path.display()))?;

    Ok(())
}

pub(super) fn limit_or_max(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "max".to_string())
}
