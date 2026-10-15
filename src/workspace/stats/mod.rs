//! Workspace resource statistics.
//!
//! Two callers exist: one workspace by name, and every running workspace for the
//! stats table. Both resolve the workspaces under the registry lock, then hand
//! the snapshots to the same collector, so the two paths cannot drift apart in
//! what they measure.
//!
//! The collector is parallel because the CPU sample sleeps for a fixed interval,
//! and that interval is per call rather than per workspace: reading twenty
//! workspaces on one thread would take twenty intervals. The workers are bounded
//! and configurable, and the results are put back in registry order so a table
//! does not reorder itself between calls.
//!
//! - `readers` reads the numbers from /proc and the workspace cgroups.
//! - `report` assembles one report from what the readers returned.

mod readers;
mod report;

use std::path::Path;

use anyhow::{anyhow, Result};

use crate::registry::with_registry;
use crate::sandbox;
use crate::sandbox::SandboxLimits;

use super::control::resolve_workspace_id;
use super::session;
use super::types::{WorkspaceMetadata, WorkspaceStatsReport};

use report::build_workspace_stats;

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
                if workspace.status.is_running() {
                    workspaces.push((workspace.clone(), sandbox.metadata.limits.clone()));
                }
            }
        }
        Ok(workspaces)
    })?;
    collect_workspace_stats(workspaces)
}

/// Read every workspace's stats, in registry order.
fn collect_workspace_stats(
    workspaces: Vec<(WorkspaceMetadata, SandboxLimits)>,
) -> Result<Vec<WorkspaceStatsReport>> {
    // A workspace whose runtime is gone keeps its place in the list with no pid,
    // so the report still says it exists and is stopped.
    let candidates = workspaces
        .into_iter()
        .map(|(workspace, sandbox_limits)| {
            let pid = workspace
                .runtime_pid
                .filter(|pid| session::process_matches(*pid, workspace.runtime_starttime_ticks));
            (workspace, sandbox_limits, pid)
        })
        .collect::<Vec<_>>();

    let pid_list: Vec<u32> = candidates.iter().filter_map(|(_, _, pid)| *pid).collect();
    let cpu_samples = readers::sample_cpu_percent(&pid_list)?;

    let jobs = candidates
        .into_iter()
        .map(|(workspace, sandbox_limits, pid)| {
            (
                workspace,
                sandbox_limits,
                pid,
                cpu_samples.get(&pid.unwrap_or_default()).copied(),
            )
        })
        .collect::<Vec<_>>();
    let workers = crate::parallel::worker_count("ENCLAVE_STATS_WORKERS", jobs.len(), 4);
    let mut reports = crate::parallel::map_bounded(jobs, workers, |job| {
        let (workspace, sandbox_limits, pid, cpu_percent) = job;
        build_workspace_stats(workspace, sandbox_limits, pid, cpu_percent)
    })
    .into_iter()
    .enumerate()
    .map(|(index, report)| {
        report
            .ok_or_else(|| anyhow::anyhow!("stats worker {index} dropped its job"))
            .and_then(|report| report)
    })
    .collect::<Result<Vec<_>>>()?;
    reports.sort_by(|a, b| {
        a.sandbox_id
            .cmp(&b.sandbox_id)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(reports)
}
