//! Cancelling a running workspace command.
//!
//! A command runs inside the workspace's namespaces under a persistent helper, so
//! its own pid is meaningless on the host. The helper itself, however, is a host
//! process, and it is the leader of a process group that every command it forks
//! inherits. Signalling that group therefore stops the command and its descendants
//! without guessing at a pid inside the workspace. The next command starts a fresh
//! helper. The daemon records the helper's pid and start time while a command is
//! in flight, and this module verifies that identity before signalling the group.
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
    let mut identity = raw.split_whitespace();
    let helper_pid: i32 = identity
        .next()
        .context("missing helper pid")?
        .parse()
        .with_context(|| format!("invalid helper pid in {}", path.display()))?;
    let helper_starttime = identity
        .next()
        .context("missing helper start time")?
        .parse::<u64>()
        .with_context(|| format!("invalid helper start time in {}", path.display()))?;
    if helper_pid <= 1 {
        bail!("refusing to signal pid {helper_pid}");
    }
    if crate::workspace::session::process_starttime_ticks(helper_pid as u32)
        .is_ok_and(|actual| actual != helper_starttime)
    {
        bail!("refusing to signal pid {helper_pid}: it changed identity");
    }
    let helper_alive =
        crate::workspace::session::process_matches(helper_pid as u32, Some(helper_starttime));
    if helper_alive {
        let group = crate::workspace::session::process_group_id(helper_pid as u32)
            .with_context(|| format!("failed to inspect helper {helper_pid} process group"))?;
        if group != helper_pid {
            bail!(
                "refusing to cancel helper {helper_pid}: it is not the leader of its process group"
            );
        }
    }

    // SIGKILL rather than SIGTERM: the helper is not the user's program and has no
    // cleanup worth waiting for, while the command it serves may ignore SIGTERM.
    // The helper leads the command's process group, so this reaches descendants
    // that inherited the group as well as the helper itself.
    if let Err(error) = crate::workspace::session::signal_process_group(
        helper_pid as u32,
        Some(helper_starttime),
        helper_pid,
        libc::SIGKILL,
    ) {
        if !helper_alive
            && !crate::workspace::process_group_has_members(helper_pid, Some(helper_starttime))
        {
            let _ = std::fs::remove_file(&path);
            return Ok(CancelOutcome {
                cancelled: false,
                already_finished: true,
            });
        }
        return Err(error).with_context(|| format!("failed to signal helper group {helper_pid}"));
    }
    if !helper_alive
        && !crate::workspace::process_group_has_members(helper_pid, Some(helper_starttime))
    {
        let _ = std::fs::remove_file(&path);
        return Ok(CancelOutcome {
            cancelled: false,
            already_finished: true,
        });
    }
    if !crate::workspace::process_group_has_members(helper_pid, Some(helper_starttime)) {
        let _ = std::fs::remove_file(&path);
        return Ok(CancelOutcome {
            cancelled: true,
            already_finished: false,
        });
    }

    // Wait for the whole group to disappear. The daemon removes the marker when the
    // request returns, but that request can be delayed by bookkeeping; the process
    // group is the thing that actually has to be gone before the next command runs.
    let deadline = Instant::now() + CANCEL_CONFIRM_TIMEOUT;
    while Instant::now() < deadline {
        if !crate::workspace::process_group_has_members(helper_pid, Some(helper_starttime)) {
            remove_marker_if_unchanged(&path, &raw);
            return Ok(CancelOutcome {
                cancelled: true,
                already_finished: false,
            });
        }
        std::thread::sleep(CANCEL_CONFIRM_POLL);
    }

    bail!(
        "command process group {} survived SIGKILL for {}s",
        helper_pid,
        CANCEL_CONFIRM_TIMEOUT.as_secs()
    )
}

/// Remove the busy marker only if it still names the command we cancelled.
///
/// A new command can publish a new marker as soon as the old group is gone, so an
/// unconditional removal here could erase the new command's cancellation handle.
fn remove_marker_if_unchanged(path: &Path, expected: &str) {
    if std::fs::read_to_string(path).is_ok_and(|current| current == expected) {
        let _ = std::fs::remove_file(path);
    }
}
