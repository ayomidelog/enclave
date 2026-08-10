use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::registry::with_registry;

use super::session;
use super::types::WorkspaceStatus;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessEntry {
    pub sandbox: String,
    pub workspace: String,
    pub status: String,
    pub uptime: Option<u64>,
    pub pid: Option<u32>,
}

pub fn list_process_status(state_dir: &std::path::Path) -> Result<Vec<ProcessEntry>> {
    let probes = with_registry(state_dir, |registry| {
        let mut probes = Vec::new();
        for sandbox in registry.sandboxes.values() {
            for workspace in sandbox.workspaces.values() {
                probes.push(ProcessProbe {
                    sandbox: sandbox.metadata.name.clone(),
                    workspace: workspace.name.clone(),
                    status: workspace.status.clone(),
                    pid: workspace.runtime_pid,
                    starttime_ticks: workspace.runtime_starttime_ticks,
                });
            }
        }
        Ok(probes)
    })?;

    let job_count = probes.len();
    let queue = Arc::new(Mutex::new(VecDeque::from_iter(
        probes.into_iter().enumerate(),
    )));
    let worker_count = configured_ps_workers(job_count);
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut entries = thread::scope(|scope| -> Result<Vec<ProcessEntry>> {
        for _ in 0..worker_count {
            let queue = Arc::clone(&queue);
            let sender = sender.clone();
            scope.spawn(move || loop {
                let job = match queue.lock() {
                    Ok(mut queue) => queue.pop_front(),
                    Err(_) => return,
                };
                let Some((index, probe)) = job else {
                    return;
                };
                let (status, pid, uptime) = resolve_live_status(&probe);
                let entry = ProcessEntry {
                    sandbox: probe.sandbox,
                    workspace: probe.workspace,
                    status,
                    uptime,
                    pid,
                };
                let _ = sender.send((index, entry));
            });
        }
        drop(sender);

        let mut ordered = (0..job_count).map(|_| None).collect::<Vec<_>>();
        for (index, entry) in receiver {
            ordered[index] = Some(entry);
        }
        Ok(ordered
            .into_iter()
            .map(|entry| entry.expect("ps worker dropped a job"))
            .collect())
    })?;

    entries.sort_by(|a, b| {
        a.sandbox
            .cmp(&b.sandbox)
            .then_with(|| a.workspace.cmp(&b.workspace))
    });
    Ok(entries)
}

fn configured_ps_workers(job_count: usize) -> usize {
    if job_count == 0 {
        return 0;
    }
    std::env::var("ENCLAVE_PS_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=64).contains(value))
        .unwrap_or(4)
        .min(job_count)
}

fn resolve_live_status(probe: &ProcessProbe) -> (String, Option<u32>, Option<u64>) {
    if probe.status != WorkspaceStatus::Running {
        return ("stopped".to_string(), None, None);
    }

    let Some(pid) = probe.pid else {
        return ("stopped".to_string(), None, None);
    };

    if !session::process_matches(pid, probe.starttime_ticks) {
        return ("stale".to_string(), None, None);
    }

    let uptime = compute_uptime(pid);
    ("running".to_string(), Some(pid), uptime)
}

#[derive(Debug, Clone)]
struct ProcessProbe {
    sandbox: String,
    workspace: String,
    status: WorkspaceStatus,
    pid: Option<u32>,
    starttime_ticks: Option<u64>,
}

fn compute_uptime(pid: u32) -> Option<u64> {
    let starttime_ticks = session::process_starttime_ticks(pid).ok()?;
    let clock_ticks_per_sec = clock_ticks_per_second()?;
    let uptime_secs = system_uptime_secs()?;

    let start_secs = starttime_ticks / clock_ticks_per_sec;
    Some(uptime_secs.saturating_sub(start_secs))
}

fn system_uptime_secs() -> Option<u64> {
    let raw = std::fs::read_to_string("/proc/uptime").ok()?;
    let first = raw.split_whitespace().next()?;
    first.parse::<f64>().ok().map(|v| v as u64)
}

fn clock_ticks_per_second() -> Option<u64> {
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks <= 0 {
        return None;
    }
    Some(ticks as u64)
}

pub fn format_uptime(total_secs: u64) -> String {
    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let minutes = (total_secs % 3600) / 60;

    if days > 0 {
        format!("{}d {}h {}m", days, hours, minutes)
    } else if hours > 0 {
        format!("{}h {}m", hours, minutes)
    } else {
        format!("{}m", minutes)
    }
}

#[cfg(test)]
#[path = "../../tests/src/workspace/ps.rs"]
mod tests;
