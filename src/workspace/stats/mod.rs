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

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

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

    let job_count = candidates.len();
    let queue = Arc::new(Mutex::new(VecDeque::from_iter(
        candidates
            .into_iter()
            .enumerate()
            .map(|(index, (workspace, sandbox_limits, pid))| {
                (
                    index,
                    workspace,
                    sandbox_limits,
                    pid,
                    cpu_samples.get(&pid.unwrap_or_default()).copied(),
                )
            }),
    )));
    let worker_count = configured_stats_workers(job_count);
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut reports = thread::scope(|scope| -> Result<Vec<WorkspaceStatsReport>> {
        for _ in 0..worker_count {
            let queue = Arc::clone(&queue);
            let sender = sender.clone();
            scope.spawn(move || loop {
                let job = match queue.lock() {
                    Ok(mut queue) => queue.pop_front(),
                    // A poisoned queue means another worker panicked; stopping
                    // leaves the collector to report the missing index.
                    Err(_) => return,
                };
                let Some((index, workspace, sandbox_limits, pid, cpu_percent)) = job else {
                    return;
                };
                let result = build_workspace_stats(workspace, sandbox_limits, pid, cpu_percent);
                let _ = sender.send((index, result));
            });
        }
        drop(sender);

        // Results arrive in completion order, so they are put back by index.
        let mut ordered = (0..job_count)
            .map(|_| None)
            .collect::<Vec<Option<Result<WorkspaceStatsReport>>>>();
        for (index, report) in receiver {
            ordered[index] = Some(report);
        }
        let mut reports = Vec::with_capacity(job_count);
        for report in ordered {
            reports.push(report.expect("stats worker dropped a job")?);
        }
        Ok(reports)
    })?;
    reports.sort_by(|a, b| {
        a.sandbox_id
            .cmp(&b.sandbox_id)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(reports)
}

/// How many workers to read the stats with.
///
/// Four by default, because the CPU sample sleeps and the work per workspace is
/// small: past a handful of workers the readers contend on /proc more than they
/// gain. Never more workers than jobs.
fn configured_stats_workers(job_count: usize) -> usize {
    if job_count == 0 {
        return 0;
    }
    std::env::var("ENCLAVE_STATS_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=64).contains(value))
        .unwrap_or(4)
        .min(job_count)
}
