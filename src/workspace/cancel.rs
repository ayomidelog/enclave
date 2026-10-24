//! Cancelling running workspace commands.
//!
//! A command runs inside the workspace's namespaces under a persistent helper, so
//! its own pid is meaningless on the host. The helper is a host process and the
//! leader of the process group every command it forks inherits. Signalling that
//! group stops the command and its descendants without guessing at a pid inside the
//! workspace.
//!
//! A workspace may run more than one command at a time, so each command publishes
//! its own marker. Cancellation locks and validates every marker before signalling,
//! and command completion holds the same lock while retiring its marker. That lock
//! is what prevents a late cancel from signalling a helper that has already been
//! returned to the pool for another command.

use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use uuid::Uuid;

/// How long to wait for cancelled command groups to disappear.
///
/// SIGKILL is immediate for a process that is not stuck in uninterruptible IO, so
/// this only has to outlast normal reaping.
const CANCEL_CONFIRM_TIMEOUT: Duration = Duration::from_secs(10);
const CANCEL_CONFIRM_POLL: Duration = Duration::from_millis(50);
const COMMAND_MARKER_DIR: &str = "commands";
const COMMAND_MARKER_EXTENSION: &str = "pgid";

/// What a cancel did.
#[derive(Debug, Clone)]
pub struct CancelOutcome {
    /// Whether a running command was found and signalled.
    pub cancelled: bool,
    /// Whether no live command remained when the cancel arrived.
    pub already_finished: bool,
}

/// The handle a running command holds for its cancellation marker.
pub(crate) struct PublishedCommandMarker {
    workspace_path: PathBuf,
    path: PathBuf,
    raw: String,
}

/// The directory containing cancellation markers for a workspace.
pub(crate) fn command_marker_dir(workspace_path: &Path) -> PathBuf {
    workspace_path.join("runtime").join(COMMAND_MARKER_DIR)
}

fn command_marker_path(workspace_path: &Path, command_id: Uuid) -> PathBuf {
    command_marker_dir(workspace_path).join(format!("{command_id}.{COMMAND_MARKER_EXTENSION}"))
}

/// The marker written by releases before per-command cancellation.
fn legacy_cancel_file_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join("runtime").join("command.pgid")
}

fn cancel_lock_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join("runtime").join("command-cancel.lock")
}

fn with_cancel_lock<T>(workspace_path: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    crate::fsutil::with_file_lock(&cancel_lock_path(workspace_path), operation)
}

/// Publish a unique marker for one command.
pub(crate) fn publish_command_marker(
    workspace_path: &Path,
    helper_pid: u32,
    starttime_ticks: u64,
) -> Result<PublishedCommandMarker> {
    with_cancel_lock(workspace_path, || {
        let path = command_marker_path(workspace_path, Uuid::new_v4());
        let raw = format!("{helper_pid} {starttime_ticks}\n");
        crate::fsutil::write_file_atomic(&path, raw.as_bytes(), 0o600)?;
        Ok(PublishedCommandMarker {
            workspace_path: workspace_path.to_path_buf(),
            path,
            raw,
        })
    })
}

/// Retire this command's marker before its helper can be reused.
///
/// Returns `false` when cancellation already removed the marker. In that case the
/// helper was signalled and must not be returned to the pool.
pub(crate) fn finish_command_marker(marker: &PublishedCommandMarker) -> Result<bool> {
    with_cancel_lock(&marker.workspace_path, || {
        let file = match OpenOptions::new().read(true).write(true).open(&marker.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to open {}", marker.path.display()))
            }
        };
        lock_marker(&file, &marker.path)?;
        let result = (|| {
            if file.metadata()?.nlink() == 0 {
                return Ok(false);
            }
            let current = fs::read_to_string(&marker.path)
                .with_context(|| format!("failed to read {}", marker.path.display()))?;
            if current != marker.raw {
                bail!(
                    "command marker {} changed while the command ran",
                    marker.path.display()
                );
            }
            fs::remove_file(&marker.path)
                .with_context(|| format!("failed to remove {}", marker.path.display()))?;
            Ok(true)
        })();
        unlock_marker(&file);
        result
    })
}

#[derive(Debug)]
struct LockedCommandMarker {
    path: PathBuf,
    raw: String,
    helper_pid: u32,
    helper_starttime_ticks: u64,
    live: bool,
    _file: File,
}

/// Stop every command running in the workspace, if any.
///
/// Every marker is parsed and identity-checked before any group is signalled. A
/// malformed or identity-mismatched marker therefore fails the request without
/// sending a signal.
///
/// `already_finished` is not an error: a cancel that races a command's own exit is
/// the caller getting what it wanted. A cancel that finds no command at all is
/// reported the same way, because from the caller's side both mean "nothing of mine
/// is running now".
pub fn cancel_workspace_command(workspace_path: &Path) -> Result<CancelOutcome> {
    with_cancel_lock(workspace_path, || cancel_locked(workspace_path))
}

fn cancel_locked(workspace_path: &Path) -> Result<CancelOutcome> {
    let markers = lock_command_markers(workspace_path)?;
    if markers.is_empty() {
        return Ok(CancelOutcome {
            cancelled: false,
            already_finished: true,
        });
    }

    let cancelled = markers.iter().any(|marker| marker.live);
    if cancelled {
        // SIGKILL rather than SIGTERM: the helper is not the user's program and has
        // no cleanup worth waiting for, while the command it serves may ignore
        // SIGTERM. The helper leads the command's process group, so this reaches
        // descendants that inherited the group as well as the helper itself.
        for marker in markers.iter().filter(|marker| marker.live) {
            crate::workspace::session::signal_process_group(
                marker.helper_pid,
                Some(marker.helper_starttime_ticks),
                marker.helper_pid as i32,
                libc::SIGKILL,
            )
            .with_context(|| format!("failed to signal helper group {}", marker.helper_pid))?;
        }

        // The daemon removes a marker when its command returns, but that request can
        // be delayed by bookkeeping. The process groups are what actually have to be
        // gone before their helpers can safely be reused.
        let deadline = Instant::now() + CANCEL_CONFIRM_TIMEOUT;
        while markers
            .iter()
            .any(|marker| marker.live && marker_is_live(marker))
        {
            if Instant::now() >= deadline {
                let survivors = markers
                    .iter()
                    .filter(|marker| marker.live && marker_is_live(marker))
                    .map(|marker| marker.helper_pid.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "command process group(s) {survivors} survived SIGKILL for {}s",
                    CANCEL_CONFIRM_TIMEOUT.as_secs()
                );
            }
            std::thread::sleep(CANCEL_CONFIRM_POLL);
        }
    }

    // The marker locks are still held, so command completion cannot return any of
    // these helpers to the pool until their markers have been retired.
    for marker in &markers {
        remove_marker_if_unchanged(&marker.path, &marker.raw);
    }
    Ok(CancelOutcome {
        cancelled,
        already_finished: !cancelled,
    })
}

/// Read and lock every current marker, then validate the complete set.
fn lock_command_markers(workspace_path: &Path) -> Result<Vec<LockedCommandMarker>> {
    let mut paths = Vec::new();
    let directory = command_marker_dir(workspace_path);
    match fs::read_dir(&directory) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str())
                    != Some(COMMAND_MARKER_EXTENSION)
                {
                    continue;
                }
                if entry.file_type()?.is_file() {
                    paths.push(path);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", directory.display()))
        }
    }
    let legacy = legacy_cancel_file_path(workspace_path);
    if legacy.is_file() {
        paths.push(legacy);
    }
    paths.sort();

    let mut markers = Vec::with_capacity(paths.len());
    for path in paths {
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("failed to open {}", path.display()))
            }
        };
        lock_marker(&file, &path)?;
        // Command completion unlinks its marker while holding this same lock. If it
        // won the race, this descriptor names a file no active command owns.
        if file.metadata()?.nlink() == 0 {
            unlock_marker(&file);
            continue;
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let (helper_pid, helper_starttime_ticks) = parse_identity(&path, &raw)?;
        validate_identity(&path, helper_pid, helper_starttime_ticks)?;
        let live = marker_is_live_values(helper_pid, helper_starttime_ticks);
        markers.push(LockedCommandMarker {
            path,
            raw,
            helper_pid,
            helper_starttime_ticks,
            live,
            _file: file,
        });
    }
    Ok(markers)
}

fn parse_identity(path: &Path, raw: &str) -> Result<(u32, u64)> {
    let mut identity = raw.split_whitespace();
    let helper_pid = identity
        .next()
        .context("missing helper pid")?
        .parse::<u32>()
        .with_context(|| format!("invalid helper pid in {}", path.display()))?;
    let helper_starttime_ticks = identity
        .next()
        .context("missing helper start time")?
        .parse::<u64>()
        .with_context(|| format!("invalid helper start time in {}", path.display()))?;
    if identity.next().is_some() {
        bail!("unexpected extra field in {}", path.display());
    }
    Ok((helper_pid, helper_starttime_ticks))
}

/// Prove a marker still names a helper this process may safely signal.
fn validate_identity(path: &Path, helper_pid: u32, helper_starttime_ticks: u64) -> Result<()> {
    if helper_pid <= 1 {
        bail!(
            "refusing to signal pid {helper_pid} from {}",
            path.display()
        );
    }
    if crate::workspace::session::process_starttime_ticks(helper_pid)
        .is_ok_and(|actual| actual != helper_starttime_ticks)
    {
        bail!("refusing to signal pid {helper_pid}: it changed identity");
    }
    if crate::workspace::session::process_matches(helper_pid, Some(helper_starttime_ticks)) {
        let group = crate::workspace::session::process_group_id(helper_pid)
            .with_context(|| format!("failed to inspect helper {helper_pid} process group"))?;
        if group != helper_pid as i32 {
            bail!(
                "refusing to cancel helper {helper_pid}: it is not the leader of its process group"
            );
        }
    }
    Ok(())
}

fn marker_is_live(marker: &LockedCommandMarker) -> bool {
    marker_is_live_values(marker.helper_pid, marker.helper_starttime_ticks)
}

fn marker_is_live_values(helper_pid: u32, helper_starttime_ticks: u64) -> bool {
    crate::workspace::session::process_matches(helper_pid, Some(helper_starttime_ticks))
        || crate::workspace::process_group_has_members(
            helper_pid as i32,
            Some(helper_starttime_ticks),
        )
}

fn lock_marker(file: &File, path: &Path) -> Result<()> {
    loop {
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error).with_context(|| format!("failed to lock {}", path.display()));
    }
}

fn unlock_marker(file: &File) {
    let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
}

/// Remove a marker only if it still contains the bytes the caller observed.
fn remove_marker_if_unchanged(path: &Path, expected: &str) {
    if fs::read_to_string(path).is_ok_and(|current| current == expected) {
        let _ = fs::remove_file(path);
    }
}
