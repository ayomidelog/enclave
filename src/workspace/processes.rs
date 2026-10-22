//! Reading the process tree a workspace owns.
//!
//! `/proc/<runtime>/task` and the cgroup's `pids.current` answer different
//! questions. The first is processes in the workspace PID namespace; the second
//! counts tasks, including every thread. A workspace with a handful of services
//! can therefore report a task count near one hundred while its process count is
//! a small single digit. Both numbers are useful, but only when they are named
//! separately.

use std::collections::BTreeMap;
use std::fs;

use anyhow::{Context, Result};

use crate::workspace::session;

/// One process visible from the host inside a workspace PID namespace.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct WorkspaceProcess {
    pub pid: u32,
    pub ppid: u32,
    #[serde(default)]
    pub process_group: u32,
    pub command: String,
    pub age_secs: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub rss_bytes: Option<u64>,
    pub threads: Option<u64>,
    pub detached: bool,
    pub job_id: Option<String>,
}

/// The host PIDs in one workspace PID namespace.
pub fn pids_in_namespace(runtime_pid: u32) -> Result<Vec<u32>> {
    let target = fs::read_link(format!("/proc/{runtime_pid}/ns/pid"))
        .with_context(|| format!("failed to read /proc/{runtime_pid}/ns/pid"))?;
    let mut pids = Vec::new();
    for entry in fs::read_dir("/proc").context("failed to read /proc")? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if fs::read_link(entry.path().join("ns/pid")).is_ok_and(|ns| ns == target) {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    Ok(pids)
}

/// Host processes in the namespace whose process group belongs to a tracked job.
///
/// A process that starts a daemon of its own may change its process group. The
/// pid namespace is still the workspace's, but the job attribution is not: only
/// the group the helper created is evidence that a process belongs to that job.
pub fn job_process_count(runtime_pid: u32, job_pids: &BTreeMap<u32, String>) -> Result<usize> {
    let pids = pids_in_namespace(runtime_pid)?;
    let mut count = 0usize;
    for pid in pids {
        let Ok(identity) = read_process_identity(pid) else {
            continue;
        };
        if job_pids.contains_key(&identity.process_group) {
            count += 1;
        }
    }
    Ok(count)
}

/// Read the process table for one workspace.
///
/// `job_pids` maps a host process-group leader to the job that launched it, so a
/// detached job is named rather than guessed from its command line. Membership in
/// the group, not merely being a non-leader process, is what makes a process
/// detached.
pub fn list_workspace_processes(
    runtime_pid: u32,
    job_pids: &BTreeMap<u32, String>,
) -> Result<Vec<WorkspaceProcess>> {
    let pids = pids_in_namespace(runtime_pid)?;
    let mut processes = Vec::with_capacity(pids.len());
    for pid in pids {
        let Ok(identity) = read_process_identity(pid) else {
            continue;
        };
        let command = read_command(pid).unwrap_or_else(|| "<unknown>".to_string());
        let rss_bytes = read_rss_bytes(pid);
        let job_id = job_pids.get(&identity.process_group).cloned();
        processes.push(WorkspaceProcess {
            pid,
            ppid: identity.ppid,
            process_group: identity.process_group,
            command,
            age_secs: identity.age_secs,
            cpu_percent: identity.cpu_percent,
            rss_bytes,
            threads: identity.threads,
            detached: job_id.is_some(),
            job_id,
        });
    }
    Ok(processes)
}

struct ProcessIdentity {
    ppid: u32,
    process_group: u32,
    age_secs: Option<u64>,
    cpu_percent: Option<f64>,
    threads: Option<u64>,
}

fn read_process_identity(pid: u32) -> Result<ProcessIdentity> {
    let raw = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let rest = raw
        .rsplit_once(") ")
        .map(|(_, rest)| rest)
        .context("stat")?;
    let fields = rest.split_whitespace().collect::<Vec<_>>();
    let ppid = fields.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
    let process_group = fields.get(2).and_then(|v| v.parse().ok()).unwrap_or(0);
    let starttime = fields.get(19).and_then(|v| v.parse::<u64>().ok());
    let age_secs = starttime.and_then(process_age_secs);
    let threads = read_threads(pid);
    let cpu_percent = read_cpu_percent(pid, age_secs);
    Ok(ProcessIdentity {
        ppid,
        process_group,
        age_secs,
        cpu_percent,
        threads,
    })
}

/// Average CPU use since the process started.
///
/// `/proc/<pid>/stat` exposes cumulative user and system ticks. Dividing those by
/// the process's age gives a useful sample for a one-shot `ps` without sleeping in
/// the daemon; the workspace stats command remains the source for an instantaneous
/// aggregate CPU rate.
fn read_cpu_percent(pid: u32, age_secs: Option<u64>) -> Option<f64> {
    let age_secs = age_secs.filter(|age| *age > 0)?;
    let raw = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = raw.rsplit_once(") ").map(|(_, rest)| rest)?;
    let fields = rest.split_whitespace().collect::<Vec<_>>();
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks <= 0 {
        return None;
    }
    Some(((utime + stime) as f64 / ticks as f64 / age_secs as f64) * 100.0)
}

fn read_threads(pid: u32) -> Option<u64> {
    let raw = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    raw.lines()
        .find_map(|line| line.strip_prefix("Threads:"))?
        .trim()
        .parse()
        .ok()
}

fn read_rss_bytes(pid: u32) -> Option<u64> {
    let raw = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let kb = raw
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    Some(kb.saturating_mul(1024))
}

fn read_command(pid: u32) -> Option<String> {
    let raw = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if raw.is_empty() {
        return None;
    }
    let command = raw
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    (!command.is_empty()).then_some(command)
}

fn process_age_secs(starttime_ticks: u64) -> Option<u64> {
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks <= 0 {
        return None;
    }
    let uptime = fs::read_to_string("/proc/uptime").ok()?;
    let uptime_secs = uptime.split_whitespace().next()?.parse::<f64>().ok()? as u64;
    Some(uptime_secs.saturating_sub(starttime_ticks / ticks as u64))
}

/// Processes attributed to a tracked background job.
pub fn detached_count(processes: &[WorkspaceProcess], runtime_pid: u32) -> usize {
    processes
        .iter()
        .filter(|process| process.pid != runtime_pid && process.job_id.is_some())
        .count()
}

/// A process group a background job belongs to, if the runtime still matches.
pub fn process_group_alive(pgid: i32, starttime_ticks: Option<u64>) -> bool {
    let Ok(pid) = u32::try_from(pgid) else {
        return false;
    };
    session::process_matches(pid, starttime_ticks)
}

/// Whether any host process still belongs to `pgid`.
///
/// The group leader can exit while a descendant that stayed in the group keeps
/// running. A TTL cancellation has to see that descendant or it will report a
/// job as cancelled while work it started is still alive.
pub fn process_group_has_members(pgid: i32, leader_starttime_ticks: Option<u64>) -> bool {
    if pgid <= 1 {
        return false;
    }
    if let (Some(expected), Some(pid)) = (leader_starttime_ticks, u32::try_from(pgid).ok()) {
        if session::process_starttime_ticks(pid).is_ok_and(|actual| actual != expected) {
            // The leader PID now names a different process. The group id may
            // have been reused, so none of its members are this job's
            // descendants.
            return false;
        }
    }
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            return false;
        };
        let Ok(pid) = name.parse::<u32>() else {
            return false;
        };
        read_process_group(pid) == Some(pgid as u32) && !process_is_zombie(pid)
    })
}

fn process_is_zombie(pid: u32) -> bool {
    let Ok(raw) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return true;
    };
    let Some(rest) = raw.rsplit_once(") ").map(|(_, rest)| rest) else {
        return true;
    };
    matches!(rest.split_whitespace().next(), Some("Z" | "X" | "x"))
}

fn read_process_group(pid: u32) -> Option<u32> {
    let raw = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = raw.rsplit_once(") ").map(|(_, rest)| rest)?;
    rest.split_whitespace().nth(2)?.parse::<u32>().ok()
}

/// Whether a host process still has the identity recorded in a job.
pub fn process_matches(pid: u32, starttime_ticks: Option<u64>) -> bool {
    session::process_matches(pid, starttime_ticks)
}

#[cfg(test)]
#[path = "../../tests/src/workspace/processes.rs"]
mod tests;
