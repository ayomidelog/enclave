//! Reading the per-process and per-cgroup numbers a stats report is built from.
//!
//! Every reader answers a question the kernel can be asked about a pid or a
//! cgroup directory, and every one of them is best effort at the call site: a
//! process that exited between the registry read and the read here is a normal
//! race, not a failure, so callers take `Ok` values and ignore errors.
//!
//! Two of them are worth explaining.
//!
//! CPU percentage is a delta, not a gauge: `/proc/<pid>/stat` holds accumulated
//! jiffies, so a sample is two reads separated by a short sleep. The percentage
//! is the process's share of the whole host's jiffies in that window, scaled by
//! the CPU count, which is what makes it comparable across hosts.
//!
//! The I/O numbers come from the cgroup when one exists, because the cgroup holds
//! every process in the workspace tree while `/proc/<pid>/io` holds only the
//! runtime leader. The process file is the fallback for a workspace whose runtime
//! is outside a cgroup.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

/// How long the CPU sample waits between its two reads.
///
/// Long enough that the jiffy counters have moved and the result is not noise,
/// short enough that a stats call stays interactive.
const CPU_SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

/// The resident size and thread count of one process.
#[derive(Debug, Clone, Default)]
pub(super) struct ProcStatusMetrics {
    pub(super) vm_rss_kb: Option<u64>,
    pub(super) threads: Option<u64>,
}

/// Bytes moved by the workspace's own network interfaces.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct NetworkStats {
    pub(super) rx_bytes: u64,
    pub(super) tx_bytes: u64,
}

/// Bytes read and written by the workspace's processes.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct IoStats {
    pub(super) read_bytes: u64,
    pub(super) write_bytes: u64,
}

/// Sample each pid's share of the host CPU over a short window.
pub(super) fn sample_cpu_percent(pids: &[u32]) -> Result<BTreeMap<u32, f64>> {
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
    // A host that recorded no jiffies at all is not busy; dividing by that would
    // report every process as infinitely hot.
    if total_delta == 0 {
        return Ok(BTreeMap::new());
    }

    let cpu_count = std::thread::available_parallelism()
        .map(|count| count.get() as f64)
        .unwrap_or(1.0);
    let mut usage = BTreeMap::new();
    for pid in pids {
        // A pid that disappeared between the two reads has no delta and is left
        // out rather than reported as zero.
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

pub(super) fn read_proc_status(pid: u32) -> Result<ProcStatusMetrics> {
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

/// Sum the traffic of every non-loopback interface in the workspace netns.
pub(super) fn read_network_stats(pid: u32) -> Result<NetworkStats> {
    let raw = fs::read_to_string(format!("/proc/{pid}/net/dev"))
        .with_context(|| format!("failed to read /proc/{pid}/net/dev"))?;
    let mut stats = NetworkStats::default();
    // The first two lines are the header, and the interface name and counters
    // are separated by a colon.
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
        // The counters are positional in the kernel's format: receive bytes
        // first, transmit bytes eighth.
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

/// Sum the I/O of every device in a cgroup, which covers the whole tree.
pub(super) fn read_cgroup_io_stats(path: &Path) -> Result<IoStats> {
    let raw = fs::read_to_string(path.join("io.stat"))
        .with_context(|| format!("failed to read {}", path.join("io.stat").display()))?;
    let mut stats = IoStats::default();
    for line in raw.lines() {
        // Each line starts with the device numbers, which are not what is
        // summed here.
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

/// The I/O of one process, used when the workspace has no cgroup.
pub(super) fn read_proc_io_stats(pid: u32) -> Result<IoStats> {
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

/// Turn a cgroup limit file into a number, where `max` means unlimited.
pub(super) fn parse_limit_value(raw: Option<&str>) -> Option<u64> {
    let raw = raw?.trim();
    if raw.is_empty() || raw == "max" {
        return None;
    }
    raw.parse::<u64>().ok()
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
    // The command name is parenthesised and can itself contain spaces, so the
    // fields are counted from the last closing parenthesis rather than the start.
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
