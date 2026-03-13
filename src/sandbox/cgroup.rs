use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::resource_limits::{cpu_quota_from_machine_percent, DEFAULT_CPU_CGROUP_PERIOD_US};

const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const MANAGED_CONTROLLERS: &[&str] = &["memory", "cpu", "pids"];

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

pub fn is_cgroup_v2_available() -> bool {
    Path::new(CGROUP_ROOT).join("cgroup.controllers").exists()
}

pub fn available_controllers() -> Vec<String> {
    let path = PathBuf::from(CGROUP_ROOT).join("cgroup.controllers");
    match fs::read_to_string(&path) {
        Ok(raw) => raw.split_whitespace().map(String::from).collect(),
        Err(err) => {
            tracing::warn!(
                "failed to read cgroup controllers from {}: {}",
                path.display(),
                err
            );
            Vec::new()
        }
    }
}

pub fn sandbox_cgroup_name(sandbox_id: &str) -> String {
    format!("enclave-sb-{sandbox_id}")
}

pub fn ensure_sandbox_cgroup(
    name: &str,
    config: &CgroupConfig,
    create_if_empty: bool,
) -> Result<Option<PathBuf>> {
    ensure_child_cgroup(
        &PathBuf::from(CGROUP_ROOT),
        name,
        config,
        create_if_empty,
        true,
    )
}

pub fn create_workspace_cgroup(name: &str, config: &CgroupConfig) -> Result<Option<PathBuf>> {
    ensure_child_cgroup(
        &PathBuf::from(CGROUP_ROOT),
        name,
        config,
        config.has_limits(),
        false,
    )
}

pub fn ensure_workspace_cgroup(
    parent: &Path,
    name: &str,
    config: &CgroupConfig,
    create_if_empty: bool,
) -> Result<Option<PathBuf>> {
    ensure_child_cgroup(parent, name, config, create_if_empty, false)
}

pub fn add_process_to_cgroup(cgroup_path: &Path, pid: u32) -> Result<()> {
    let procs_file = cgroup_path.join("cgroup.procs");
    write_cgroup_value(&procs_file, &pid.to_string())
        .with_context(|| format!("failed to add pid {} to cgroup", pid))
}

pub fn apply_cgroup_limits(cgroup_path: &Path, config: &CgroupConfig) -> Result<()> {
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

pub fn read_cgroup_stats(cgroup_path: &Path) -> Result<CgroupStats> {
    let memory_current = read_cgroup_u64(&cgroup_path.join("memory.current"));
    let memory_max = read_cgroup_string(&cgroup_path.join("memory.max"));
    let pids_current = read_cgroup_u64(&cgroup_path.join("pids.current"));
    let pids_max = read_cgroup_string(&cgroup_path.join("pids.max"));
    let cpu_max = read_cgroup_string(&cgroup_path.join("cpu.max"));

    Ok(CgroupStats {
        memory_current_bytes: memory_current,
        memory_max_bytes: memory_max,
        pids_current,
        pids_max,
        cpu_max,
    })
}

pub fn runtime_cgroup_path(pid: u32) -> Result<Option<PathBuf>> {
    let raw = fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .with_context(|| format!("failed to read /proc/{pid}/cgroup"))?;
    for line in raw.lines() {
        let mut parts = line.splitn(3, ':');
        let hierarchy = parts.next().unwrap_or_default();
        let controllers = parts.next().unwrap_or_default();
        let relative_path = parts.next().unwrap_or_default();
        if hierarchy != "0" || !controllers.is_empty() {
            continue;
        }
        let trimmed = relative_path.trim_start_matches('/');
        if trimmed.is_empty() {
            return Ok(Some(PathBuf::from(CGROUP_ROOT)));
        }
        return Ok(Some(PathBuf::from(CGROUP_ROOT).join(trimmed)));
    }
    Ok(None)
}

pub fn remove_workspace_cgroup(name: &str) -> Result<()> {
    validate_cgroup_name(name)?;
    remove_cgroup_path(&PathBuf::from(CGROUP_ROOT).join(name))
}

pub fn remove_cgroup_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
            tracing::warn!(
                "cgroup {} is not empty; kernel will clean up on process exit",
                path.display()
            );
            Ok(())
        }
        Err(err) => Err(err).with_context(|| format!("failed to remove cgroup {}", path.display())),
    }
}

#[derive(Debug, Clone, Default)]
pub struct CgroupStats {
    pub memory_current_bytes: Option<u64>,
    pub memory_max_bytes: Option<String>,
    pub pids_current: Option<u64>,
    pub pids_max: Option<String>,
    pub cpu_max: Option<String>,
}

impl std::fmt::Display for CgroupStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "memory={}/{}, pids={}/{}, cpu_max={}",
            self.memory_current_bytes
                .map_or("unknown".to_string(), |v| format!("{}B", v)),
            self.memory_max_bytes.as_deref().unwrap_or("max"),
            self.pids_current
                .map_or("unknown".to_string(), |v| v.to_string()),
            self.pids_max.as_deref().unwrap_or("max"),
            self.cpu_max.as_deref().unwrap_or("max"),
        )
    }
}

fn ensure_child_cgroup(
    parent: &Path,
    name: &str,
    config: &CgroupConfig,
    create_if_empty: bool,
    enable_children: bool,
) -> Result<Option<PathBuf>> {
    if !is_cgroup_v2_available() {
        return Ok(None);
    }
    if !config.has_limits() && !create_if_empty {
        return Ok(None);
    }

    validate_cgroup_name(name)?;
    enable_managed_controllers(&parent.join("cgroup.subtree_control"));

    let cgroup_path = parent.join(name);
    fs::create_dir_all(&cgroup_path)
        .with_context(|| format!("failed to create cgroup {}", cgroup_path.display()))?;
    apply_cgroup_limits(&cgroup_path, config)?;
    if enable_children {
        enable_managed_controllers(&cgroup_path.join("cgroup.subtree_control"));
    }

    Ok(Some(cgroup_path))
}

fn enable_managed_controllers(subtree_control: &Path) {
    for controller in MANAGED_CONTROLLERS {
        if let Err(err) = write_cgroup_value(subtree_control, &format!("+{controller}")) {
            tracing::warn!("failed to enable {} controller: {err:#}", controller);
        }
    }
}

fn limit_or_max(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "max".to_string())
}

fn validate_cgroup_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("cgroup name must not be empty");
    }
    if name.starts_with('/') {
        bail!("cgroup name must not be absolute: {name}");
    }
    if name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        bail!("cgroup name contains unsafe path components: {name}");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        bail!("cgroup name contains disallowed characters: {name}");
    }
    Ok(())
}

fn write_cgroup_value(path: &Path, value: &str) -> Result<()> {
    fs::write(path, value)
        .with_context(|| format!("failed to write '{}' to {}", value, path.display()))
}

fn read_cgroup_u64(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse::<u64>().ok()
}

fn read_cgroup_string(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

#[cfg(test)]
#[path = "../../tests/src/sandbox/cgroup.rs"]
mod tests;
