use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::registry::with_registry;
use crate::sandbox;
use crate::sandbox::SandboxLimits;

use super::control::resolve_workspace_id;
use super::session;
use super::types::{WorkspaceMetadata, WorkspaceStatsReport, WorkspaceStatus};

const CPU_SAMPLE_INTERVAL: Duration = Duration::from_millis(200);
pub fn workspace_stats(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<WorkspaceStatsReport> {
    let workspace = with_registry(state_dir, |registry| {
        let sandbox_id = sandbox::resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        Ok((workspace, sandbox.metadata.limits.clone()))
    })?;
    collect_workspace_stats(vec![workspace])?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("workspace stats unavailable"))
}

pub fn list_running_workspace_stats(state_dir: &Path) -> Result<Vec<WorkspaceStatsReport>> {
    let workspaces = with_registry(state_dir, |registry| {
        let mut workspaces = Vec::new();
        for sandbox in registry.sandboxes.values() {
            for workspace in sandbox.workspaces.values() {
                if workspace.status == WorkspaceStatus::Running {
                    workspaces.push((workspace.clone(), sandbox.metadata.limits.clone()));
                }
            }
        }
        Ok(workspaces)
    })?;
    collect_workspace_stats(workspaces)
}

fn collect_workspace_stats(
    workspaces: Vec<(WorkspaceMetadata, SandboxLimits)>,
) -> Result<Vec<WorkspaceStatsReport>> {
    let mut candidates = Vec::new();
    for (workspace, sandbox_limits) in workspaces {
        let Some(pid) = workspace.runtime_pid else {
            candidates.push((workspace, sandbox_limits, None));
            continue;
        };
        if session::process_matches(pid, workspace.runtime_starttime_ticks) {
            candidates.push((workspace, sandbox_limits, Some(pid)));
        } else {
            candidates.push((workspace, sandbox_limits, None));
        }
    }

    let pid_list: Vec<u32> = candidates.iter().filter_map(|(_, _, pid)| *pid).collect();
    let cpu_samples = sample_cpu_percent(&pid_list)?;

    let mut reports = Vec::with_capacity(candidates.len());
    for (workspace, sandbox_limits, pid) in candidates {
        reports.push(build_workspace_stats(
            workspace,
            sandbox_limits,
            pid,
            cpu_samples.get(&pid.unwrap_or_default()).copied(),
        )?);
    }
    reports.sort_by(|a, b| {
        a.sandbox_id
            .cmp(&b.sandbox_id)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(reports)
}

fn build_workspace_stats(
    workspace: WorkspaceMetadata,
    sandbox_limits: SandboxLimits,
    pid: Option<u32>,
    cpu_percent: Option<f64>,
) -> Result<WorkspaceStatsReport> {
    let active_process_count = pid
        .and_then(|runtime_pid| session::count_processes_in_pid_namespace(runtime_pid).ok())
        .unwrap_or(0);
    let proc_status = pid.and_then(|runtime_pid| read_proc_status(runtime_pid).ok());
    let cgroup_path = pid
        .map(crate::sandbox::cgroup::runtime_cgroup_path)
        .transpose()?
        .flatten();
    let cgroup_stats = cgroup_path
        .as_ref()
        .and_then(|path| crate::sandbox::cgroup::read_cgroup_stats(path).ok());
    let io_stats = cgroup_path
        .as_ref()
        .and_then(|path| read_cgroup_io_stats(path).ok())
        .or_else(|| pid.and_then(|runtime_pid| read_proc_io_stats(runtime_pid).ok()));
    let net_stats = pid.and_then(|runtime_pid| read_network_stats(runtime_pid).ok());

    let memory_usage_bytes = cgroup_stats
        .as_ref()
        .and_then(|stats| stats.memory_current_bytes)
        .or_else(|| {
            proc_status
                .as_ref()
                .and_then(|status| status.vm_rss_kb.map(|v| v * 1024))
        });
    let memory_limit_bytes = cgroup_stats
        .as_ref()
        .and_then(|stats| parse_limit_value(stats.memory_max_bytes.as_deref()))
        .or(workspace.limits.memory_bytes);
    let memory_percent = match (memory_usage_bytes, memory_limit_bytes) {
        (Some(usage), Some(limit)) if limit > 0 => Some((usage as f64 / limit as f64) * 100.0),
        _ => None,
    };
    let pids_current = cgroup_stats
        .as_ref()
        .and_then(|stats| stats.pids_current)
        .or_else(|| u64::try_from(active_process_count).ok());
    let pids_limit = cgroup_stats
        .as_ref()
        .and_then(|stats| parse_limit_value(stats.pids_max.as_deref()))
        .or(workspace.limits.max_processes);

    Ok(WorkspaceStatsReport {
        id: workspace.id,
        sandbox_id: workspace.sandbox_id,
        name: workspace.name,
        status: if pid.is_some() {
            WorkspaceStatus::Running
        } else {
            WorkspaceStatus::Stopped
        },
        pid,
        active_process_count,
        threads: proc_status.as_ref().and_then(|status| status.threads),
        cpu_percent,
        cpu_limit_percent: workspace.limits.cpu_percent,
        sandbox_cpu_limit_percent: sandbox_limits.cpu_percent,
        memory_usage_bytes,
        memory_limit_bytes,
        sandbox_memory_limit_bytes: sandbox_limits.memory_bytes,
        memory_percent,
        net_rx_bytes: net_stats.as_ref().map(|stats| stats.rx_bytes),
        net_tx_bytes: net_stats.as_ref().map(|stats| stats.tx_bytes),
        block_read_bytes: io_stats.as_ref().map(|stats| stats.read_bytes),
        block_write_bytes: io_stats.as_ref().map(|stats| stats.write_bytes),
        pids_current,
        pids_limit,
    })
}

fn sample_cpu_percent(pids: &[u32]) -> Result<BTreeMap<u32, f64>> {
    if pids.is_empty() {
        return Ok(BTreeMap::new());
    }

    let first_total = read_total_cpu_jiffies()?;
    let first_pid: BTreeMap<u32, u64> = pids
        .iter()
        .filter_map(|pid| {
            read_process_cpu_jiffies(*pid)
                .ok()
                .map(|value| (*pid, value))
        })
        .collect();
    thread::sleep(CPU_SAMPLE_INTERVAL);
    let second_total = read_total_cpu_jiffies()?;
    let second_pid: BTreeMap<u32, u64> = pids
        .iter()
        .filter_map(|pid| {
            read_process_cpu_jiffies(*pid)
                .ok()
                .map(|value| (*pid, value))
        })
        .collect();
    let total_delta = second_total.saturating_sub(first_total);
    if total_delta == 0 {
        return Ok(BTreeMap::new());
    }

    let cpu_count = std::thread::available_parallelism()
        .map(|count| count.get() as f64)
        .unwrap_or(1.0);
    let mut usage = BTreeMap::new();
    for pid in pids {
        let Some(start) = first_pid.get(pid) else {
            continue;
        };
        let Some(end) = second_pid.get(pid) else {
            continue;
        };
        let pid_delta = end.saturating_sub(*start);
        let percent = (pid_delta as f64 / total_delta as f64) * cpu_count * 100.0;
        usage.insert(*pid, percent);
    }
    Ok(usage)
}

fn read_total_cpu_jiffies() -> Result<u64> {
    let raw = fs::read_to_string("/proc/stat").context("failed to read /proc/stat")?;
    let first_line = raw
        .lines()
        .next()
        .ok_or_else(|| anyhow!("missing aggregate cpu line in /proc/stat"))?;
    let total = first_line
        .split_whitespace()
        .skip(1)
        .try_fold(0u64, |acc, part| {
            let value = part.parse::<u64>()?;
            Ok::<u64, anyhow::Error>(acc.saturating_add(value))
        })?;
    Ok(total)
}

fn read_process_cpu_jiffies(pid: u32) -> Result<u64> {
    let stat_path = format!("/proc/{pid}/stat");
    let raw =
        fs::read_to_string(&stat_path).with_context(|| format!("failed to read {}", stat_path))?;
    let right_paren = raw
        .rfind(')')
        .ok_or_else(|| anyhow!("failed to parse {}", stat_path))?;
    let rest = raw
        .get((right_paren + 2)..)
        .ok_or_else(|| anyhow!("failed to parse fields in {}", stat_path))?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    if fields.len() < 15 {
        return Err(anyhow!("unexpected field count in {}", stat_path));
    }
    let utime = fields[11].parse::<u64>()?;
    let stime = fields[12].parse::<u64>()?;
    Ok(utime.saturating_add(stime))
}

fn parse_limit_value(raw: Option<&str>) -> Option<u64> {
    let raw = raw?.trim();
    if raw.is_empty() || raw == "max" {
        return None;
    }
    raw.parse::<u64>().ok()
}

#[derive(Debug, Clone, Default)]
struct ProcStatusMetrics {
    vm_rss_kb: Option<u64>,
    threads: Option<u64>,
}

fn read_proc_status(pid: u32) -> Result<ProcStatusMetrics> {
    let status_path = format!("/proc/{pid}/status");
    let raw = fs::read_to_string(&status_path)
        .with_context(|| format!("failed to read {}", status_path))?;
    let mut metrics = ProcStatusMetrics::default();
    for line in raw.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            metrics.vm_rss_kb = value.split_whitespace().next().and_then(|v| v.parse().ok());
        } else if let Some(value) = line.strip_prefix("Threads:") {
            metrics.threads = value.trim().parse::<u64>().ok();
        }
    }
    Ok(metrics)
}

#[derive(Debug, Clone, Copy, Default)]
struct NetworkStats {
    rx_bytes: u64,
    tx_bytes: u64,
}

fn read_network_stats(pid: u32) -> Result<NetworkStats> {
    let raw = fs::read_to_string(format!("/proc/{pid}/net/dev"))
        .with_context(|| format!("failed to read /proc/{pid}/net/dev"))?;
    let mut stats = NetworkStats::default();
    for line in raw.lines().skip(2) {
        let mut parts = line.split(':');
        let interface = parts.next().map(str::trim).unwrap_or_default();
        if interface.is_empty() || interface == "lo" {
            continue;
        }
        let Some(values) = parts.next() else {
            continue;
        };
        let columns: Vec<&str> = values.split_whitespace().collect();
        if columns.len() < 16 {
            continue;
        }
        stats.rx_bytes = stats
            .rx_bytes
            .saturating_add(columns[0].parse::<u64>().unwrap_or(0));
        stats.tx_bytes = stats
            .tx_bytes
            .saturating_add(columns[8].parse::<u64>().unwrap_or(0));
    }
    Ok(stats)
}

#[derive(Debug, Clone, Copy, Default)]
struct IoStats {
    read_bytes: u64,
    write_bytes: u64,
}

fn read_cgroup_io_stats(path: &Path) -> Result<IoStats> {
    let raw = fs::read_to_string(path.join("io.stat"))
        .with_context(|| format!("failed to read {}", path.join("io.stat").display()))?;
    let mut stats = IoStats::default();
    for line in raw.lines() {
        for field in line.split_whitespace().skip(1) {
            if let Some(value) = field.strip_prefix("rbytes=") {
                stats.read_bytes = stats.read_bytes.saturating_add(value.parse::<u64>()?);
            } else if let Some(value) = field.strip_prefix("wbytes=") {
                stats.write_bytes = stats.write_bytes.saturating_add(value.parse::<u64>()?);
            }
        }
    }
    Ok(stats)
}

fn read_proc_io_stats(pid: u32) -> Result<IoStats> {
    let raw = fs::read_to_string(format!("/proc/{pid}/io"))
        .with_context(|| format!("failed to read /proc/{pid}/io"))?;
    let mut stats = IoStats::default();
    for line in raw.lines() {
        if let Some(value) = line.strip_prefix("read_bytes:") {
            stats.read_bytes = value.trim().parse::<u64>()?;
        } else if let Some(value) = line.strip_prefix("write_bytes:") {
            stats.write_bytes = value.trim().parse::<u64>()?;
        }
    }
    Ok(stats)
}
