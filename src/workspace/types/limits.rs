//! What a workspace is allowed to use, and how a caller changes that.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::resource_limits::{validate_cpu_percent, validate_memory_bytes};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceLimits {
    pub cpu_seconds: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<u64>,
    pub max_processes: Option<u64>,
    pub max_open_files: Option<u64>,
    pub disk_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceLimitsUpdate {
    pub clear_tmp_on_restart: Option<bool>,
    pub keep_running: Option<bool>,
    pub idle_stop_after_secs: Option<Option<u64>>,
    pub destroy_after_secs: Option<Option<u64>>,
    pub cpu_seconds: Option<Option<u64>>,
    pub cpu_percent: Option<Option<f64>>,
    pub memory_bytes: Option<Option<u64>>,
    pub max_processes: Option<Option<u64>>,
    pub max_open_files: Option<Option<u64>>,
    pub disk_bytes: Option<Option<u64>>,
}

impl WorkspaceLimits {
    /// Fill an omitted CPU share or process cap from the daemon defaults.
    ///
    /// The resolved values are stored on the workspace record, so a later start,
    /// resize, and cgroup write all enforce the same effective limit. Applying a
    /// default only at cgroup creation would make the record and the running
    /// runtime disagree.
    pub fn with_daemon_defaults(self) -> Self {
        self.apply_defaults(
            crate::workspace::default_workspace_cpu_percent(),
            crate::workspace::default_workspace_max_processes(),
        )
    }

    /// Apply explicit defaults. Kept separate from the process-wide getters so
    /// the merge is testable without changing daemon-global state.
    pub fn apply_defaults(mut self, cpu_percent: Option<f64>, max_processes: Option<u64>) -> Self {
        self.cpu_percent = self.cpu_percent.or(cpu_percent);
        self.max_processes = self.max_processes.or(max_processes);
        self
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(cpu_percent) = self.cpu_percent {
            validate_cpu_percent(cpu_percent)?;
        }
        if self.max_processes == Some(0) {
            anyhow::bail!("max_processes must be greater than 0");
        }
        validate_memory_bytes(self.memory_bytes)?;
        Ok(())
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
        if let Some(disk_bytes) = update.disk_bytes {
            changed |= self.disk_bytes != disk_bytes;
            self.disk_bytes = disk_bytes;
        }
        self.validate()?;
        Ok(changed)
    }
}

impl WorkspaceLimitsUpdate {
    pub fn is_empty(&self) -> bool {
        self.clear_tmp_on_restart.is_none()
            && self.keep_running.is_none()
            && self.idle_stop_after_secs.is_none()
            && self.destroy_after_secs.is_none()
            && self.cpu_seconds.is_none()
            && self.cpu_percent.is_none()
            && self.memory_bytes.is_none()
            && self.max_processes.is_none()
            && self.max_open_files.is_none()
            && self.disk_bytes.is_none()
    }
}
