//! Daemon-wide defaults and limits for idle and capacity management.
//!
//! These are policy rather than per-workspace state: the workspace record may
//! override its own idle and destroy windows, but the daemon decides what an
//! undeclared workspace inherits and what the host-wide capacity is.

use std::time::Duration;

use anyhow::{bail, Result};

use crate::resource_limits::validate_cpu_percent;

#[derive(Debug, Clone, Default)]
pub struct IdlePolicy {
    /// Default idle-stop window. `None` disables idle stops.
    pub idle_stop_after_secs: Option<u64>,
    /// Default destroy-after window for stopped workspaces. `None` keeps files.
    pub destroy_after_secs: Option<u64>,
    /// Default CPU share for workspaces that do not declare one.
    pub default_workspace_cpu_percent: Option<f64>,
    /// Default process cap for workspaces that do not declare one.
    pub default_workspace_max_processes: Option<u64>,
    /// Task count doctor treats as abnormal for one workspace.
    pub task_count_threshold: u64,
    /// Number of processes beyond the runtime leader doctor reports.
    pub detached_process_threshold: u64,
    /// Daemon-wide cap on running workspace runtimes.
    pub max_running_workspaces: Option<usize>,
    /// How often the sweeper runs.
    pub sweep_interval: Duration,
    /// Whether the background sweeper is started with the daemon.
    pub sweep_enabled: bool,
    /// Whether the sweep should stop idle workspaces or merely report them.
    pub evict_idle: bool,
}

impl IdlePolicy {
    pub fn validate(&self) -> Result<()> {
        if let Some(percent) = self.default_workspace_cpu_percent {
            validate_cpu_percent(percent)?;
        }
        if self.default_workspace_max_processes == Some(0) {
            bail!("default_workspace_max_processes must be greater than 0");
        }
        if self.task_count_threshold == 0 {
            bail!("task_count_threshold must be greater than 0");
        }
        if self.max_running_workspaces == Some(0) {
            bail!("max_running_workspaces must be greater than 0");
        }
        if self.sweep_interval.is_zero() {
            bail!("sweep_interval must be greater than 0");
        }
        Ok(())
    }
}
