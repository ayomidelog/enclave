//! Cancelling a running workspace command.
//!
//! A command runs inside the workspace's namespaces under a persistent helper, so
//! its own pid is meaningless on the host. The helper itself, however, is a host
//! process, and it holds the workspace's command mutex for exactly as long as the
//! command runs. Killing the helper therefore stops the command and frees the
//! workspace; the next command starts a fresh helper. The daemon records the
//! helper's pid while a command is in flight, and this module signals it.
//!
//! This is what makes `run_command_stop` mean something. Without it the caller
//! could only kill its own local CLI process, while the command kept running,
//! kept its exec worker, and kept the workspace's command mutex - so the next
//! command for that workspace queued behind work nobody was waiting for any more.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

/// How long to wait for a cancelled command to disappear.
///
/// SIGKILL is immediate for a process that is not stuck in uninterruptible IO, so
/// this only has to outlast normal reaping.
const CANCEL_CONFIRM_TIMEOUT: Duration = Duration::from_secs(10);
const CANCEL_CONFIRM_POLL: Duration = Duration::from_millis(50);

/// Where a workspace records the helper serving its running command.
///
/// Kept beside the workspace's runtime files rather than in a global directory, so
/// it is removed with the workspace and cannot collide between workspaces.
pub(crate) fn cancel_file_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join("runtime").join("command.pgid")
}

/// What a cancel did.
#[derive(Debug, Clone)]
pub struct CancelOutcome {
    /// Whether a running command was found and signalled.
    pub cancelled: bool,
    /// Whether the command had already finished when the cancel arrived.
    pub already_finished: bool,
}

/// Stop the command `workspace_path` is running, if any.
///
/// `already_finished` is not an error: a cancel that races a command's own exit is
/// the caller getting what it wanted. A cancel that finds no command at all is
/// reported the same way, because from the caller's side both mean "nothing of
/// mine is running now".
pub fn cancel_workspace_command(workspace_path: &Path) -> Result<CancelOutcome> {
    let path = cancel_file_path(workspace_path);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CancelOutcome {
                cancelled: false,
                already_finished: true,
            })
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()))
        }
    };
    let helper_pid: i32 = raw
        .trim()
        .parse()
        .with_context(|| format!("invalid helper pid in {}", path.display()))?;
    if helper_pid <= 1 {
        bail!("refusing to signal pid {helper_pid}");
    }

    // SIGKILL rather than SIGTERM: the helper is not the user's program and has no
    // cleanup worth waiting for, while the command it serves may ignore SIGTERM.
    // The workspace's storage is released when the helper dies, which is what makes
    // the next command start cleanly.
    let result = unsafe { libc::kill(helper_pid, libc::SIGKILL) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            let _ = std::fs::remove_file(&path);
            return Ok(CancelOutcome {
                cancelled: false,
                already_finished: true,
            });
        }
        return Err(error).with_context(|| format!("failed to signal helper {helper_pid}"));
    }

    // Wait for the helper to disappear so the caller's next command does not race a
    // command that is still winding down. The daemon removes the marker once the
    // request it is serving returns, which is the signal that the workspace is free.
    let deadline = Instant::now() + CANCEL_CONFIRM_TIMEOUT;
    while Instant::now() < deadline {
        if !path.exists() {
            return Ok(CancelOutcome {
                cancelled: true,
                already_finished: false,
            });
        }
        std::thread::sleep(CANCEL_CONFIRM_POLL);
    }

    // The signal was delivered, so the cancel itself succeeded. The file lingering
    // means the helper has not reaped the command yet; report the cancel rather
    // than pretending it failed.
    Ok(CancelOutcome {
        cancelled: true,
        already_finished: false,
    })
}
