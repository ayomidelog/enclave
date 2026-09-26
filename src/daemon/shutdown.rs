//! Daemon shutdown: signals, the drain deadline, and the report.
//!
//! Shutdown stops accepting requests, waits a bounded time for the lifecycle
//! operations already running, and then names whatever is left. Their journals
//! survive, so the next daemon start finishes or rolls back the interrupted work;
//! the report is what makes that visible at the time.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet, Signal};

use super::active_operations;

/// Set by a shutdown signal or by the shutdown request.
pub(in crate::daemon) static SIGNAL_SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// The write end of the pipe that wakes the accept loop, or `-1` before it exists.
///
/// The accept loop waits in `poll` with no timeout, so nothing would otherwise
/// interrupt it when a shutdown arrives. A signal already interrupts the wait, but
/// the handler only runs between the wait and the next `poll`, and a shutdown
/// requested over the socket arrives on a worker thread where no signal is
/// delivered at all. Both paths write one byte here.
static SHUTDOWN_WAKEUP_WRITE_FD: std::sync::atomic::AtomicI32 =
    std::sync::atomic::AtomicI32::new(-1);

/// Wake the accept loop out of its wait so it can notice a shutdown.
///
/// This is called from a signal handler, so it may only use async-signal-safe
/// calls: `write` is one, and the value it writes is a constant.
pub(in crate::daemon) fn wake_shutdown_wait() {
    let fd = SHUTDOWN_WAKEUP_WRITE_FD.load(Ordering::SeqCst);
    if fd < 0 {
        return;
    }
    let byte = [1u8];
    unsafe { libc::write(fd, byte.as_ptr().cast(), 1) };
}

/// Create the pipe the accept loop waits on and return its read end.
///
/// Called once, before any worker can request a shutdown, so the handler's load of
/// the write end can never see a stale descriptor from a previous daemon in this
/// process.
pub(in crate::daemon) fn install_shutdown_wait() -> Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to create the daemon shutdown wakeup pipe");
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    SHUTDOWN_WAKEUP_WRITE_FD.store(write_fd, Ordering::SeqCst);
    // The read end is owned by the caller and closed with it.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(read_fd) })
}

pub(super) fn shutdown_requested(shutdown: &Arc<AtomicBool>) -> bool {
    shutdown.load(Ordering::SeqCst) || SIGNAL_SHUTDOWN.load(Ordering::SeqCst)
}

/// How long shutdown waits for running operations before leaving them behind.
///
/// Long enough for a stop, a destroy, or a resize to finish; short enough that
/// a stuck or very long operation cannot hold the daemon open. The journals
/// make the abandoned work recoverable, so waiting forever buys nothing.
pub(super) fn shutdown_grace() -> Duration {
    crate::deadlines::daemon_shutdown_grace().get()
}

/// Name every operation shutdown had to abandon, with how to recover it.
pub(super) fn report_incomplete_operations(
    remaining: &[active_operations::ActiveOperation],
    grace: Duration,
) {
    if remaining.is_empty() {
        tracing::info!("daemon shutdown: no lifecycle operation was left running");
        return;
    }
    for operation in remaining {
        tracing::error!(
            operation_id = %operation.id,
            action = %operation.action,
            target = %operation.target,
            elapsed_secs = operation.elapsed().as_secs(),
            "daemon shutdown left an operation running after {}s; its journal is retained",
            grace.as_secs()
        );
    }
    tracing::error!(
        "daemon shutdown report: {} operation(s) incomplete; the next daemon start finishes or rolls them back",
        remaining.len()
    );
}

pub(super) fn install_signal_handlers() -> Result<()> {
    SIGNAL_SHUTDOWN.store(false, Ordering::SeqCst);
    let action = SigAction::new(
        SigHandler::Handler(handle_shutdown_signal),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );

    unsafe {
        signal::sigaction(Signal::SIGINT, &action).context("failed to register SIGINT handler")?;
        signal::sigaction(Signal::SIGTERM, &action)
            .context("failed to register SIGTERM handler")?;
    }
    Ok(())
}

extern "C" fn handle_shutdown_signal(_: i32) {
    SIGNAL_SHUTDOWN.store(true, Ordering::SeqCst);
    wake_shutdown_wait();
}
