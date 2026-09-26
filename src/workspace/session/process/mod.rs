use std::fs;
use std::io;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

mod signal;

pub(super) use signal::{send_signal, verify_signal_target, SignalTarget};

pub fn process_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

pub fn process_matches(pid: u32, expected_starttime_ticks: Option<u64>) -> bool {
    match read_stat(pid) {
        Ok(stat) => {
            !stat.has_exited()
                && expected_starttime_ticks.is_none_or(|expected| expected == stat.starttime_ticks)
        }
        Err(_) => false,
    }
}

pub fn read_namespace_refs(pid: u32) -> Result<(String, String)> {
    let mount = fs::read_link(format!("/proc/{pid}/ns/mnt"))
        .with_context(|| format!("failed to read /proc/{pid}/ns/mnt"))?
        .to_string_lossy()
        .to_string();
    let pid_ns = fs::read_link(format!("/proc/{pid}/ns/pid"))
        .with_context(|| format!("failed to read /proc/{pid}/ns/pid"))?
        .to_string_lossy()
        .to_string();
    Ok((mount, pid_ns))
}

pub(super) fn read_namespace_identity(pid: u32) -> Result<[String; 5]> {
    Ok([
        fs::read_link(format!("/proc/{pid}/ns/user"))?
            .to_string_lossy()
            .to_string(),
        fs::read_link(format!("/proc/{pid}/ns/mnt"))?
            .to_string_lossy()
            .to_string(),
        fs::read_link(format!("/proc/{pid}/ns/pid"))?
            .to_string_lossy()
            .to_string(),
        fs::read_link(format!("/proc/{pid}/ns/net"))?
            .to_string_lossy()
            .to_string(),
        fs::read_link(format!("/proc/{pid}/ns/uts"))?
            .to_string_lossy()
            .to_string(),
    ])
}

pub fn count_processes_in_pid_namespace(pid: u32) -> Result<usize> {
    let target = fs::read_link(format!("/proc/{pid}/ns/pid"))
        .with_context(|| format!("failed to read /proc/{pid}/ns/pid"))?;

    let mut count = 0usize;
    for entry in fs::read_dir("/proc").context("failed to read /proc")? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }

        let ns_path = format!("/proc/{name}/ns/pid");
        let ns = match fs::read_link(&ns_path) {
            Ok(ns) => ns,
            Err(_) => continue,
        };

        if ns == target {
            count += 1;
        }
    }

    Ok(count)
}

pub fn process_resource_usage(pid: u32) -> Result<String> {
    let status_path = format!("/proc/{pid}/status");
    let raw = fs::read_to_string(&status_path)
        .with_context(|| format!("failed to read {}", status_path))?;

    let mut vm_rss_kb: Option<u64> = None;
    let mut vm_size_kb: Option<u64> = None;
    let mut threads: Option<u64> = None;

    for line in raw.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            vm_rss_kb = parse_status_kb_value(value);
        } else if let Some(value) = line.strip_prefix("VmSize:") {
            vm_size_kb = parse_status_kb_value(value);
        } else if let Some(value) = line.strip_prefix("Threads:") {
            threads = value.trim().parse::<u64>().ok();
        }
    }

    Ok(format!(
        "vm_rss_kb={}, vm_size_kb={}, threads={}",
        vm_rss_kb.map_or_else(|| "unknown".to_string(), |v| v.to_string()),
        vm_size_kb.map_or_else(|| "unknown".to_string(), |v| v.to_string()),
        threads.map_or_else(|| "unknown".to_string(), |v| v.to_string())
    ))
}

struct ProcessStat {
    state: char,
    starttime_ticks: u64,
}

impl ProcessStat {
    /// A process that already exited keeps a `/proc` entry until its parent
    /// reaps it. It owns no runtime resources, so it must never be treated as
    /// a live workspace runtime.
    fn has_exited(&self) -> bool {
        matches!(self.state, 'Z' | 'X' | 'x')
    }
}

fn read_stat(pid: u32) -> Result<ProcessStat> {
    let stat_path = format!("/proc/{pid}/stat");
    let raw =
        fs::read_to_string(&stat_path).with_context(|| format!("failed to read {}", stat_path))?;
    let right_paren = raw
        .rfind(')')
        .ok_or_else(|| anyhow!("failed to parse stat format in {}", stat_path))?;
    let rest = raw
        .get((right_paren + 2)..)
        .ok_or_else(|| anyhow!("failed to parse stat fields in {}", stat_path))?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    if fields.len() < 20 {
        bail!("unexpected stat field count in {}", stat_path);
    }

    let state = fields[0]
        .chars()
        .next()
        .ok_or_else(|| anyhow!("missing state field in {}", stat_path))?;
    let starttime_ticks = fields[19]
        .parse::<u64>()
        .with_context(|| format!("failed to parse starttime field in {}", stat_path))?;
    Ok(ProcessStat {
        state,
        starttime_ticks,
    })
}

pub fn process_starttime_ticks(pid: u32) -> Result<u64> {
    Ok(read_stat(pid)?.starttime_ticks)
}

pub(crate) fn read_pid_file(path: &Path) -> Result<u32> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let pid = raw.trim().parse::<u32>().map_err(|_| {
        anyhow!(
            "invalid pid file {} content '{}'",
            path.display(),
            raw.trim()
        )
    })?;
    Ok(pid)
}

pub(super) fn read_log_tail(path: &Path, max_lines: usize) -> Result<String> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let lines: Vec<&str> = raw.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    Ok(lines[start..].join("\n"))
}

fn parse_status_kb_value(raw: &str) -> Option<u64> {
    raw.split_whitespace().next()?.parse::<u64>().ok()
}

/// The command lines a workspace runtime is allowed to have.
///
/// A runtime is one process that becomes a series of them: the launcher that
/// unshares the namespaces, the init that records the pid and the namespace
/// references, the bootstrap helper that mounts and pivots, and finally the session
/// loop. Every step keeps the pid the daemon recorded, so every step has to be
/// recognized, not only the last one. Recognizing only the loop and the bootstrap
/// meant a stop that arrived while the runtime was still in its init step refused to
/// signal it, cleared the record, and left the runtime running with its cgroup, its
/// interface, and its mounts owned by nothing.
///
/// `enclave-workspace-session` is not one of the steps: it is the name the test
/// fixtures give a process to make it look like a runtime.
const ENCLAVE_RUNTIME_CMDLINE_MARKERS: [&str; 5] = [
    "enclave-workspace-session",
    "workspace-session-launch",
    "workspace-session-init",
    "workspace-session-bootstrap",
    "workspace-session-loop",
];

fn looks_like_enclave_runtime_cmdline(cmdline: &str) -> bool {
    ENCLAVE_RUNTIME_CMDLINE_MARKERS
        .iter()
        .any(|marker| cmdline.contains(marker))
}

fn current_euid() -> u32 {
    unsafe { libc::geteuid() as u32 }
}

pub(super) fn workspace_runtime_hostname(name: &str) -> String {
    let mut out = String::new();
    let mut previous_dash = false;
    for c in name.chars() {
        let lowered = c.to_ascii_lowercase();
        if lowered.is_ascii_alphanumeric() {
            out.push(lowered);
            previous_dash = false;
        } else if !previous_dash {
            out.push('-');
            previous_dash = true;
        }
        if out.len() >= 63 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        return "workspace".to_string();
    }
    trimmed
}

#[cfg(test)]
#[path = "../../../../tests/src/workspace/session/process.rs"]
mod tests;
