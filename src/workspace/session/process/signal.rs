//! Signalling a workspace runtime, and refusing to signal one this daemon
//! does not own.
//!
//! Every path that ends a runtime goes through here, so the check that the pid
//! still names the process the record describes lives in one place rather than
//! being repeated by each caller.

use super::*;

pub(in crate::workspace::session) fn send_signal(pid: u32, signal: i32) -> Result<()> {
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

/// Whether a recorded pid is one this process may signal.
///
/// A pid that is alive but is not something this process may end means the record
/// that named it is stale, which is a different outcome from the check itself
/// failing. Callers decide what to do with that, so the distinction is a type
/// rather than a substring of the message. One of the outcomes is a moment rather
/// than a verdict, because a runtime that was just spawned does not look like one
/// yet, and only the caller knows whether waiting is worth it.
pub(in crate::workspace::session) enum SignalTarget {
    /// The pid is an Enclave runtime this process owns.
    Signallable,
    /// The pid is alive and matches the record, but its command line does not
    /// name a runtime.
    ///
    /// A process carries its launcher's command line from the `fork` that created
    /// it until the `exec` that makes it the runtime has finished, and while that
    /// `exec` is in progress `/proc/<pid>/cmdline` reads as empty or as a
    /// half-copied argument vector. A runtime is therefore in this state for a
    /// moment after it is spawned, and a process that is not a runtime is in it for
    /// as long as it lives. Waiting is the only thing that tells the two apart, so
    /// the wait belongs to the caller: this outcome reports what the pid looks like
    /// now, and the caller decides how long to wait before it calls the record
    /// stale.
    Starting,
    /// The pid belongs to another user, so it cannot be a runtime this process
    /// started and the record that named it is stale.
    ForeignOwner { owner_uid: u32 },
}

impl SignalTarget {
    pub(in crate::workspace::session) fn is_signallable(&self) -> bool {
        matches!(self, Self::Signallable)
    }
}

pub(in crate::workspace::session) fn verify_signal_target(
    pid: u32,
    expected_starttime_ticks: Option<u64>,
) -> Result<SignalTarget> {
    if !process_matches(pid, expected_starttime_ticks) {
        return Ok(SignalTarget::Signallable);
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
        return Ok(SignalTarget::ForeignOwner { owner_uid });
    }

    let cmdline_path = format!("/proc/{pid}/cmdline");
    let cmdline =
        fs::read(&cmdline_path).with_context(|| format!("failed to read {}", cmdline_path))?;
    let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
    if !looks_like_enclave_runtime_cmdline(&cmdline) {
        return Ok(SignalTarget::Starting);
    }

    Ok(SignalTarget::Signallable)
}
