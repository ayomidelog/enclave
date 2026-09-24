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

mod active_operations;
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
    let workers = match serve(listener, &config, &shutdown, &services) {
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
) -> Result<workers::RequestWorkerPool> {
    listener
        .set_nonblocking(true)
        .context("failed to make daemon listener nonblocking")?;
    let workers = workers::RequestWorkerPool::new(config, shutdown, services)?;
    loop {
        if shutdown_requested(shutdown) {
            break;
        }

        wait_for_listener(&listener)?;
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

fn wait_for_listener(listener: &UnixListener) -> Result<()> {
    // The poll wakes once a second even when nothing connects. That is the
    // price of noticing a shutdown request while blocked in the accept loop: a
    // self-pipe would let the poll block indefinitely, but one wakeup per second
    // costs nothing next to the thread and socket machinery it would add.
    let mut readiness = libc::pollfd {
        fd: std::os::fd::AsRawFd::as_raw_fd(listener),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(&mut readiness, 1, 1_000) };
        if result > 0 {
            if readiness.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                bail!(
                    "daemon listener became unusable (poll events 0x{:x})",
                    readiness.revents
                );
            }
            return Ok(());
        }
        if result == 0 {
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
