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
/// A pid that is alive but is not an Enclave runtime this process owns means the
/// record that named it is stale, which is a different outcome from the check
/// itself failing. Callers decide what to do with that, so the distinction is a
/// type rather than a substring of the message.
pub(in crate::workspace::session) enum SignalTarget {
    /// The pid is an Enclave runtime this process owns.
    Signallable,
    /// The pid is not an Enclave runtime this process owns.
    Stale(StaleTarget),
}

/// Why a recorded pid is not signallable.
pub(in crate::workspace::session) enum StaleTarget {
    /// The pid belongs to another user.
    ForeignOwner { owner_uid: u32 },
    /// The pid exists but its command line is not an Enclave runtime.
    NotEnclaveProcess,
}

impl std::fmt::Display for StaleTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ForeignOwner { owner_uid } => write!(formatter, "owned by uid {owner_uid}"),
            Self::NotEnclaveProcess => write!(formatter, "not an enclave runtime process"),
        }
    }
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
        return Ok(SignalTarget::Stale(StaleTarget::ForeignOwner { owner_uid }));
    }

    let cmdline_path = format!("/proc/{pid}/cmdline");
    let cmdline =
        fs::read(&cmdline_path).with_context(|| format!("failed to read {}", cmdline_path))?;
    let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
    if !looks_like_enclave_runtime_cmdline(&cmdline) {
        return Ok(SignalTarget::Stale(StaleTarget::NotEnclaveProcess));
    }

    Ok(SignalTarget::Signallable)
}
