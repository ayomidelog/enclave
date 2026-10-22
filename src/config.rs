use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileConfig {
    pub socket: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub pid_file: Option<PathBuf>,
    pub debootstrap_binary: Option<String>,
    pub workspace_apparmor_profile: Option<String>,
    pub workspace_selinux_label: Option<String>,
    pub suite: Option<String>,
    pub mirror: Option<String>,
    pub wait_secs: Option<u64>,
    pub bootstrap_method: Option<String>,
    /// Stop an idle workspace after this many seconds. Zero disables idle stops.
    pub idle_stop_after_secs: Option<u64>,
    /// Destroy a stopped workspace after this many seconds. Zero keeps it.
    pub destroy_after_secs: Option<u64>,
    /// Default CPU share for a workspace that does not declare one.
    pub default_workspace_cpu_percent: Option<f64>,
    /// Default process cap for a workspace that does not declare one.
    pub default_workspace_max_processes: Option<u64>,
    pub task_count_threshold: Option<u64>,
    pub detached_process_threshold: Option<u64>,
    /// Most workspace runtimes that may be running across the daemon.
    pub max_running_workspaces: Option<usize>,
    /// How often the daemon checks idle and destroy policy.
    pub sweep_interval_secs: Option<u64>,
    /// Whether the activity sweep runs.
    pub sweep_enabled: Option<bool>,
    /// Whether idle policy is enforced or only reported by doctor.
    pub evict_idle: Option<bool>,
}

pub fn load_config(explicit_path: Option<&Path>) -> Result<FileConfig> {
    let Some(path) = resolve_config_path(explicit_path) else {
        return Ok(FileConfig::default());
    };

    if !path.exists() {
        if explicit_path.is_some() {
            anyhow::bail!(
                "config file not found at {} (specified via --config)",
                path.display()
            );
        }
        return Ok(FileConfig::default());
    }

    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    let parsed: FileConfig = toml::from_str(&raw)
        .with_context(|| format!("failed to parse config {}", path.display()))?;
    // The context names the file only. Naming a field here would be wrong for most
    // of what this validates: the AppArmor and SELinux labels are rejected by the
    // same call, and the inner error already names the field it is about.
    validate_file_config(&parsed)
        .with_context(|| format!("invalid values in config {}", path.display()))?;
    Ok(parsed)
}

fn validate_file_config(config: &FileConfig) -> Result<()> {
    for (label, value) in [
        (
            "workspace_apparmor_profile",
            config.workspace_apparmor_profile.as_deref(),
        ),
        (
            "workspace_selinux_label",
            config.workspace_selinux_label.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            if value.trim().is_empty() || value.chars().any(char::is_control) {
                anyhow::bail!("{label} must be a non-empty label without control characters");
            }
        }
    }

    let suite = config
        .suite
        .as_deref()
        .unwrap_or(crate::sandbox::DEFAULT_DEBIAN_SUITE);
    let mirror = config
        .mirror
        .as_deref()
        .unwrap_or(crate::sandbox::DEFAULT_DEBIAN_MIRROR);
    crate::sandbox::validate_debootstrap_inputs(suite, mirror)?;
    if let Some(cpu_percent) = config.default_workspace_cpu_percent {
        crate::resource_limits::validate_cpu_percent(cpu_percent)?;
    }
    if config.default_workspace_max_processes == Some(0) {
        anyhow::bail!("default_workspace_max_processes must be greater than 0");
    }
    if config.task_count_threshold == Some(0) {
        anyhow::bail!("task_count_threshold must be greater than 0");
    }
    if config.max_running_workspaces == Some(0) {
        anyhow::bail!("max_running_workspaces must be greater than 0");
    }
    if config.sweep_interval_secs == Some(0) {
        anyhow::bail!("sweep_interval_secs must be greater than 0");
    }
    Ok(())
}

fn resolve_config_path(explicit_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit_path {
        return Some(path.to_path_buf());
    }
    if let Ok(config_home) = env::var("XDG_CONFIG_HOME") {
        return Some(
            PathBuf::from(config_home)
                .join("enclave")
                .join("config.toml"),
        );
    }
    if let Ok(home) = env::var("HOME") {
        return Some(
            PathBuf::from(home)
                .join(".config")
                .join("enclave")
                .join("config.toml"),
        );
    }
    None
}

#[cfg(test)]
#[path = "../tests/src/config.rs"]
mod tests;
