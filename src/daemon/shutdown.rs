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
}
