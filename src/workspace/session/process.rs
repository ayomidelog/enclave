use std::fs;
use std::io;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

pub fn process_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

pub fn process_matches(pid: u32, expected_starttime_ticks: Option<u64>) -> bool {
    if !process_alive(pid) {
        return false;
    }
    match expected_starttime_ticks {
        Some(expected) => process_starttime_ticks(pid)
            .map(|actual| actual == expected)
            .unwrap_or(false),
        None => true,
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

pub fn process_starttime_ticks(pid: u32) -> Result<u64> {
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

    fields[19]
        .parse::<u64>()
        .with_context(|| format!("failed to parse starttime field in {}", stat_path))
}

pub(super) fn send_signal(pid: u32, signal: i32) -> Result<()> {
    let rc = unsafe { libc::kill(pid as i32, signal) };
    if rc == 0 {
        return Ok(());
    }

    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }

    Err(anyhow!(
        "failed to send signal {} to pid {}: {}",
        signal,
        pid,
        err
    ))
}

pub(super) fn verify_signal_target(pid: u32, expected_starttime_ticks: Option<u64>) -> Result<()> {
    if !process_matches(pid, expected_starttime_ticks) {
        return Ok(());
    }

    let status_path = format!("/proc/{pid}/status");
    let status = fs::read_to_string(&status_path)
        .with_context(|| format!("failed to read {}", status_path))?;
    let uid_line = status
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .ok_or_else(|| anyhow!("missing Uid line in {}", status_path))?;
    let owner_uid = uid_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("failed to parse uid from {}", status_path))?
        .parse::<u32>()
        .with_context(|| format!("failed to parse uid in {}", status_path))?;
    let current_uid = current_euid();
    if current_uid != 0 && owner_uid != current_uid {
        bail!(
            "refusing to signal pid {} owned by uid {} (current uid {})",
            pid,
            owner_uid,
            current_uid
        );
    }

    let cmdline_path = format!("/proc/{pid}/cmdline");
    let cmdline =
        fs::read(&cmdline_path).with_context(|| format!("failed to read {}", cmdline_path))?;
    let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
    if !looks_like_enclave_runtime_cmdline(&cmdline) {
        bail!(
            "refusing to signal pid {} because it does not look like an enclave runtime process",
            pid
        );
    }

    Ok(())
}

pub(super) fn read_pid_file(path: &Path) -> Result<u32> {
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

fn looks_like_enclave_runtime_cmdline(cmdline: &str) -> bool {
    cmdline.contains("enclave-workspace-session")
        || cmdline.contains("workspace-session-loop")
        || cmdline.contains("workspace-session-bootstrap")
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
#[path = "../../../tests/src/workspace/session/process.rs"]
mod tests;
