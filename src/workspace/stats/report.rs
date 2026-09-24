//! Assembling one workspace's stats report from what the readers returned.
//!
//! Every field is a preference order rather than a single source. The cgroup is
//! authoritative when the workspace has one, because it covers the whole process
//! tree and holds the limit the kernel is enforcing; the runtime process and the
//! configured workspace limits are the fallbacks. That order matters: a workspace
//! whose cgroup is gone should still report something useful rather than nothing,
//! and a field whose sources are all absent stays `None` instead of being
//! reported as zero.

use anyhow::Result;

use crate::sandbox::SandboxLimits;

use super::super::session;
use super::super::types::{WorkspaceMetadata, WorkspaceStatsReport, WorkspaceStatus};
use super::readers::{
    parse_limit_value, read_cgroup_io_stats, read_network_stats, read_proc_io_stats,
    read_proc_status, IoStats, NetworkStats, ProcStatusMetrics,
};

pub(super) fn build_workspace_stats(
    workspace: WorkspaceMetadata,
    sandbox_limits: SandboxLimits,
    pid: Option<u32>,
    cpu_percent: Option<f64>,
) -> Result<WorkspaceStatsReport> {
    // Every read below is best effort: a process that exited between the
    // registry read and this call is a normal race, and the report still has to
    // be produced for the workspaces that are still running.
    let active_process_count = pid
        .and_then(|runtime_pid| session::count_processes_in_pid_namespace(runtime_pid).ok())
        .unwrap_or(0);
    let proc_status: Option<ProcStatusMetrics> =
        pid.and_then(|runtime_pid| read_proc_status(runtime_pid).ok());
    let cgroup_path = pid
        .map(crate::sandbox::cgroup::runtime_cgroup_path)
        .transpose()?
        .flatten();
    let cgroup_stats = cgroup_path
        .as_ref()
        .and_then(|path| crate::sandbox::cgroup::read_cgroup_stats(path).ok());
    let io_stats: Option<IoStats> = cgroup_path
        .as_ref()
        .and_then(|path| read_cgroup_io_stats(path).ok())
        .or_else(|| pid.and_then(|runtime_pid| read_proc_io_stats(runtime_pid).ok()));
    let net_stats: Option<NetworkStats> =
        pid.and_then(|runtime_pid| read_network_stats(runtime_pid).ok());

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
        // A limit of zero would make the ratio meaningless, so it is reported as
        // unknown rather than as an infinite percentage.
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
        // The status reported here is what the numbers were read from, not what
        // the registry says: a workspace whose runtime is gone reports stopped
        // even if the registry has not caught up.
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
