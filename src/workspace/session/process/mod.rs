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

/// The process-group id a live host process belongs to.
pub fn process_group_id(pid: u32) -> Result<i32> {
    let raw = fs::read_to_string(format!("/proc/{pid}/stat"))
        .with_context(|| format!("failed to read /proc/{pid}/stat"))?;
    let rest = raw
        .rsplit_once(") ")
        .map(|(_, rest)| rest)
        .with_context(|| format!("failed to parse /proc/{pid}/stat"))?;
    rest.split_whitespace()
        .nth(2)
        .with_context(|| format!("/proc/{pid}/stat has no process-group field"))?
        .parse::<i32>()
        .with_context(|| format!("failed to parse the process-group field of /proc/{pid}/stat"))
}

/// Signal a process group after proving its leader is the recorded process.
pub fn signal_process_group(
    leader_pid: u32,
    starttime_ticks: Option<u64>,
    pgid: i32,
    signal: i32,
) -> Result<()> {
    if pgid <= 1 || i32::try_from(leader_pid).ok() != Some(pgid) {
        bail!("refusing to signal process group {pgid}");
    }
    if process_matches(leader_pid, starttime_ticks) {
        signal_whole_group(pgid, signal)?;
        return Ok(());
    }

    // The group can outlive its leader. If the leader pid now names another
    // process, the group id may have been reused and is no longer ours. A
    // zombie is the leader we are still waiting to reap, so it is safe to
    // inspect the remaining members.
    if let Ok(stat) = read_stat(leader_pid) {
        if !stat.has_exited() {
            bail!("refusing to signal process group {pgid}: leader {leader_pid} changed identity");
        }
        if starttime_ticks.is_some_and(|expected| stat.starttime_ticks != expected) {
            bail!("refusing to signal process group {pgid}: leader {leader_pid} changed identity");
        }
    }

    let members = live_process_group_members(pgid)?;
    if members.is_empty() {
        return Ok(());
    }
    for pid in members {
        signal_group_member(pid, pgid, signal)?;
    }
    Ok(())
}

fn signal_whole_group(pgid: i32, signal: i32) -> Result<()> {
    let rc = unsafe { libc::kill(-pgid, signal) };
    if rc == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(error).with_context(|| format!("failed to signal process group {pgid}"))
}

/// Live (non-zombie) host processes in one process group.
///
/// A process that has exited but has not been reaped still appears in `/proc`.
/// It cannot run, and counting it as a member would keep a job alive forever.
fn live_process_group_members(pgid: i32) -> Result<Vec<u32>> {
    if pgid <= 1 {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir("/proc").context("failed to read /proc")?;
    let mut members = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let Ok(stat) = read_stat(pid) else {
            continue;
        };
        if stat.has_exited() || read_process_group(pid) != Some(pgid as u32) {
            continue;
        }
        members.push(pid);
    }
    Ok(members)
}

/// Signal one process only after proving it still belongs to the group.
///
/// A pid can be recycled between the `/proc` scan and the signal. A pidfd makes
/// the identity stable when the kernel supports it; the fallback rechecks the
/// group and state immediately before signalling. Either way, a process that
/// left the group or became a zombie is left alone.
fn signal_group_member(pid: u32, pgid: i32, signal: i32) -> Result<()> {
    let still_member = read_stat(pid)
        .is_ok_and(|stat| !stat.has_exited() && read_process_group(pid) == Some(pgid as u32));
    if !still_member {
        return Ok(());
    }
    if let Some(fd) = crate::pidfd::open_pidfd(pid) {
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd,
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        let error = (result != 0).then(io::Error::last_os_error);
        unsafe { libc::close(fd) };
        match error {
            None => return Ok(()),
            Some(error) if error.raw_os_error() == Some(libc::ESRCH) => return Ok(()),
            Some(error) if error.raw_os_error() == Some(libc::ENOSYS) => {}
            Some(error) => {
                return Err(error)
                    .with_context(|| format!("failed to signal pid {pid} in process group {pgid}"))
            }
        }
    }
    let rc = unsafe { libc::kill(pid as i32, signal) };
    if rc == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(error).with_context(|| format!("failed to signal pid {pid} in process group {pgid}"))
}

fn read_process_group(pid: u32) -> Option<u32> {
    let raw = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = raw.rsplit_once(") ").map(|(_, rest)| rest)?;
    rest.split_whitespace().nth(2)?.parse::<u32>().ok()
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
