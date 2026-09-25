//! The sandbox daemon: its entry point, its accept loop, and its lifecycle.
//!
//! The daemon owns the configured state directory through a lock file, serves
//! newline-delimited JSON requests over a Unix socket, and is the only process
//! that mutates the registry or the host resources a workspace owns. Each concern
//! lives in its own module:
//!
//! - `socket` prepares and removes the socket and pid file.
//! - `request` handles one client request, including its operation id.
//! - `shutdown` owns the signals, the drain deadline, and the report.
//! - `workers` runs the bounded request worker pools.
//! - `leases` serializes overlapping lifecycle operations.
//! - `active_operations` records the operations running right now.
//! - `ports` re-establishes the published ports of a running workspace.

pub(crate) mod active_operations;
mod dispatch;
mod leases;
mod ports;
mod rate_limiter;
mod request;
mod services;
mod shutdown;
mod socket;
pub(crate) mod state_lock;
mod workers;

use std::fs;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::policy;
use crate::sandbox;

use rate_limiter::RateLimiter;
use shutdown::shutdown_requested;

const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(2);
const RATE_LIMIT_MAX_REQUESTS: usize = 120;
const RATE_LIMIT_GLOBAL_MAX_REQUESTS: usize = 600;

/// Longest shutdown waits for a worker to finish a request it already started.
///
/// The lifecycle drain has its own, longer deadline; by the time this runs the
/// only work left is a request that ignored the drain or a read that will not
/// return. Waiting longer than a moment for those buys nothing.
const WORKER_JOIN_GRACE: Duration = Duration::from_secs(1);

/// Everything the daemon needs to know about where it lives.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub socket_path: PathBuf,
    pub state_dir: PathBuf,
    pub pid_file: PathBuf,
    pub debootstrap_binary: String,
    pub workspace_apparmor_profile: Option<String>,
    pub workspace_selinux_label: Option<String>,
}

pub fn run_daemon(config: DaemonConfig) -> Result<()> {
    shutdown::install_signal_handlers()?;
    let _state_lock = state_lock::acquire_state_lock(&config.state_dir, &config.socket_path)?;
    sandbox::init_storage(&config.state_dir)?;
    // Resolve transitions the previous daemon left half-done. This is the one
    // place it is safe: the state lock above proved no other daemon is running
    // and this one has not served a request yet, so nothing is in flight to roll
    // back. Every later `init_storage` call is a create, which must leave a
    // concurrent start alone.
    crate::sandbox::reconcile_runtime_state(&config.state_dir)?;
    // Every open journal record at this point belongs to a previous daemon: the
    // state lock above proved no other daemon is running, and this one has not
    // served a request yet. The reconciliation above resolved the targets it
    // could, so the records are closed here with that outcome. Without this the
    // journal keeps every interrupted operation open forever, and doctor reports
    // each one as unfinished on every run.
    match crate::operation::close_unfinished_records(
        &config.state_dir,
        "interrupted by a daemon restart; the target state was reconciled at startup",
    ) {
        Ok(closed) if !closed.is_empty() => {
            tracing::warn!(
                "closed {} operation journal record(s) left open by a previous daemon: {}",
                closed.len(),
                closed.join(", ")
            );
        }
        Ok(_) => {}
        // A journal that cannot be updated is worth reporting but not worth
        // refusing to start over: the daemon can still serve, and doctor will
        // name the records that stayed open.
        Err(error) => tracing::warn!("failed to close unfinished operation records: {error:#}"),
    }
    // The journal is one file per lifecycle operation and nothing else trims it, so
    // a host that starts and stops workspaces for months would accumulate them
    // without bound. This runs after the close above, so every record left is
    // terminal and the newest are the operations a reader is most likely to ask
    // about.
    match crate::operation::prune_terminal_records(&config.state_dir) {
        Ok(0) => {}
        Ok(removed) => tracing::info!(
            "trimmed {removed} terminal operation journal record(s) beyond the retention limit"
        ),
        Err(error) => tracing::warn!("failed to trim the operation journal: {error:#}"),
    }
    policy::ensure_policy(&config.state_dir)?;
    socket::prepare_runtime_paths(&config.socket_path, &config.pid_file)?;
    let services = services::DaemonServices {
        rate_limiter: Arc::new(RateLimiter::with_global_limit(
            RATE_LIMIT_WINDOW,
            RATE_LIMIT_MAX_REQUESTS,
            RATE_LIMIT_GLOBAL_MAX_REQUESTS,
        )),
        port_publisher: Arc::new(crate::network::publish::PortPublisher::new()),
        leases: Arc::new(leases::LifecycleLeases::default()),
        active_operations: Arc::new(active_operations::ActiveOperations::default()),
    };
    ports::reconcile_published_ports(&config.state_dir, &services.port_publisher)?;

    let listener = UnixListener::bind(&config.socket_path)
        .with_context(|| format!("failed to bind socket {}", config.socket_path.display()))?;
    fs::set_permissions(&config.socket_path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set mode on {}", config.socket_path.display()))?;
    crate::fsutil::verify_secure_socket(&config.socket_path)?;
    fs::write(&config.pid_file, std::process::id().to_string())
        .with_context(|| format!("failed to write pid file {}", config.pid_file.display()))?;

    let shutdown = Arc::new(AtomicBool::new(false));
    // Created before the accept loop and before any worker can request a shutdown,
    // so the pipe the signal handler writes to always exists by the time it runs.
    let shutdown_wait = shutdown::install_shutdown_wait()?;
    let workers = match serve(listener, &config, &shutdown, &services, &shutdown_wait) {
        Ok(workers) => workers,
        Err(err) => {
            tracing::error!("daemon accept loop failed: {err:#}");
            // Drain and clean up before returning the error, so a failed accept
            // loop does not leave a stale socket that looks like a live daemon.
            finish_shutdown(&services, None);
            if let Err(cleanup) = socket::cleanup_files(&config.socket_path, &config.pid_file) {
                tracing::warn!("daemon cleanup failed: {cleanup:#}");
            }
            return Err(err);
        }
    };

    finish_shutdown(&services, Some(workers));

    drop(services);
    crate::network::cleanup_host_networking();

    if let Err(err) = socket::cleanup_files(&config.socket_path, &config.pid_file) {
        tracing::warn!("daemon cleanup failed: {err:#}");
    }
    Ok(())
}

/// Drain the running operations, report what did not finish, and join the workers.
///
/// Shutdown stops accepting new requests, waits a bounded time for the operations
/// already running, and then names whatever is left. Their journals survive, so
/// the next daemon start finishes or rolls back the interrupted work; this report
/// is what makes that visible now.
fn finish_shutdown(
    services: &services::DaemonServices,
    workers: Option<workers::RequestWorkerPool>,
) {
    let grace = shutdown::shutdown_grace();
    let remaining = services.active_operations.wait_for_drain(grace);
    shutdown::report_incomplete_operations(&remaining, grace);

    if let Some(workers) = workers {
        let unfinished = workers.finish_within(WORKER_JOIN_GRACE);
        if unfinished > 0 {
            tracing::error!(
                "daemon shutdown left {} request worker(s) still running",
                unfinished
            );
        }
    }
}

fn serve(
    listener: UnixListener,
    config: &DaemonConfig,
    shutdown: &Arc<AtomicBool>,
    services: &services::DaemonServices,
    shutdown_wait: &OwnedFd,
) -> Result<workers::RequestWorkerPool> {
    listener
        .set_nonblocking(true)
        .context("failed to make daemon listener nonblocking")?;
    let workers = workers::RequestWorkerPool::new(config, shutdown, services)?;
    loop {
        if shutdown_requested(shutdown) {
            break;
        }

        wait_for_listener(&listener, shutdown_wait)?;
        if shutdown_requested(shutdown) {
            break;
        }

        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                continue;
            }
            Err(error) => {
                tracing::warn!("sandbox daemon accept error: {error}");
                continue;
            }
        };

        workers.submit(stream)?;
    }

    Ok(workers)
}

/// Wait until a client connects or a shutdown is requested, without polling.
///
/// The wait blocks indefinitely and ends on either event, so an idle daemon does
/// no work at all and a shutdown is noticed at once rather than at the next
/// periodic wakeup. The shutdown side is a pipe: the signal handler and the worker
/// that serves a shutdown request both write one byte to it, which is the only
/// thing a signal handler is allowed to do to interrupt a wait.
fn wait_for_listener(listener: &UnixListener, shutdown_wait: &OwnedFd) -> Result<()> {
    let mut descriptors = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: shutdown_wait.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        let result = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) };
        if result > 0 {
            if descriptors[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                bail!(
                    "daemon listener became unusable (poll events 0x{:x})",
                    descriptors[0].revents
                );
            }
            if descriptors[1].revents != 0 {
                // Drain the byte, so a wakeup that has already been acted on does
                // not make the next wait return immediately.
                let mut byte = [0u8; 1];
                unsafe { libc::read(descriptors[1].fd, byte.as_mut_ptr().cast(), 1) };
            }
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error).context("failed to wait for daemon listener readiness");
    }
}

#[cfg(test)]
#[path = "../../tests/src/daemon/mod.rs"]
mod tests;
