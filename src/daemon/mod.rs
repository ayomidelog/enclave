mod dispatch;
mod rate_limiter;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::policy;
use crate::protocol::{Request, Response};
use crate::sandbox;
use crate::workspace::WorkspaceStatus;
use anyhow::{bail, Context, Result};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};

use rate_limiter::RateLimiter;

const MAX_REQUEST_BYTES: usize = 64 * 1024;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(2);
const RATE_LIMIT_MAX_REQUESTS: usize = 120;
const RATE_LIMIT_GLOBAL_MAX_REQUESTS: usize = 600;
static SIGNAL_SHUTDOWN: AtomicBool = AtomicBool::new(false);

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
    install_signal_handlers()?;
    sandbox::init_storage(&config.state_dir)?;
    policy::ensure_policy(&config.state_dir)?;
    prepare_runtime_paths(&config.socket_path, &config.pid_file)?;
    let port_publisher = Arc::new(crate::network::publish::PortPublisher::new());
    reconcile_published_ports(&config.state_dir, &port_publisher)?;

    let listener = UnixListener::bind(&config.socket_path)
        .with_context(|| format!("failed to bind socket {}", config.socket_path.display()))?;
    fs::set_permissions(&config.socket_path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set mode on {}", config.socket_path.display()))?;
    crate::fsutil::verify_secure_socket(&config.socket_path)?;
    fs::write(&config.pid_file, std::process::id().to_string())
        .with_context(|| format!("failed to write pid file {}", config.pid_file.display()))?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let rate_limiter = Arc::new(RateLimiter::with_global_limit(
        RATE_LIMIT_WINDOW,
        RATE_LIMIT_MAX_REQUESTS,
        RATE_LIMIT_GLOBAL_MAX_REQUESTS,
    ));
    let serve_result = serve(listener, &config, &shutdown, &rate_limiter, &port_publisher);

    drop(port_publisher);
    crate::network::cleanup_host_networking();

    if let Err(err) = cleanup_files(&config.socket_path, &config.pid_file) {
        tracing::warn!("daemon cleanup failed: {err:#}");
    }
    serve_result
}

fn prepare_runtime_paths(socket_path: &Path, pid_file: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        crate::fsutil::ensure_secure_dir(parent)?;
    }
    if let Some(parent) = pid_file.parent() {
        crate::fsutil::ensure_secure_dir(parent)?;
    }
    if socket_path.exists() {
        let metadata = fs::symlink_metadata(socket_path)
            .with_context(|| format!("failed to stat {}", socket_path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!(
                "refusing to use symlink socket path {}",
                socket_path.display()
            );
        }
        if !metadata.file_type().is_socket() {
            bail!(
                "refusing to use non-socket path {} for daemon socket",
                socket_path.display()
            );
        }

        match UnixStream::connect(socket_path) {
            Ok(_) => {
                bail!(
                    "socket path {} is active; stop the running daemon first",
                    socket_path.display()
                );
            }
            Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => {
                fs::remove_file(socket_path).with_context(|| {
                    format!("failed to remove stale socket {}", socket_path.display())
                })?;
            }
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                bail!(
                    "permission denied while probing existing socket {}: {}",
                    socket_path.display(),
                    err
                );
            }
            Err(err) => {
                bail!(
                    "failed to probe existing socket {}: {}",
                    socket_path.display(),
                    err
                );
            }
        }
    }
    Ok(())
}

fn cleanup_files(socket_path: &Path, pid_file: &Path) -> Result<()> {
    if socket_path.exists() {
        fs::remove_file(socket_path)
            .with_context(|| format!("failed to remove socket {}", socket_path.display()))?;
    }
    if pid_file.exists() {
        fs::remove_file(pid_file)
            .with_context(|| format!("failed to remove pid file {}", pid_file.display()))?;
    }
    Ok(())
}

fn serve(
    listener: UnixListener,
    config: &DaemonConfig,
    shutdown: &Arc<AtomicBool>,
    rate_limiter: &Arc<RateLimiter>,
    port_publisher: &Arc<crate::network::publish::PortPublisher>,
) -> Result<()> {
    for stream in listener.incoming() {
        if shutdown_requested(shutdown) {
            break;
        }

        let mut stream = match stream {
            Ok(stream) => stream,
            Err(err) => {
                if shutdown_requested(shutdown) {
                    break;
                }
                tracing::warn!("sandbox daemon accept error: {err}");
                continue;
            }
        };

        if let Err(err) = handle_client(&mut stream, config, shutdown, rate_limiter, port_publisher)
        {
            tracing::warn!("sandbox daemon request error: {err}");
        }

        if shutdown_requested(shutdown) {
            break;
        }
    }

    Ok(())
}

fn handle_client(
    stream: &mut UnixStream,
    config: &DaemonConfig,
    shutdown: &Arc<AtomicBool>,
    rate_limiter: &Arc<RateLimiter>,
    port_publisher: &Arc<crate::network::publish::PortPublisher>,
) -> Result<()> {
    let request_raw = match read_request_line(stream) {
        Ok(Some(request_raw)) => request_raw,
        Ok(None) => return Ok(()),
        Err(err) => {
            let response = Response::err(err.to_string());
            write_response(stream, &response)?;
            return Ok(());
        }
    };

    let request: Request = match serde_json::from_str(&request_raw) {
        Ok(request) => request,
        Err(err) => {
            let response = Response::err(format!("invalid request payload: {err}"));
            write_response(stream, &response)?;
            return Ok(());
        }
    };
    let peer_uid = peer_uid(stream).context("failed to resolve peer uid")?;
    if !rate_limiter.allow(peer_uid) {
        let response = Response::err(format!(
            "rate limit exceeded for uid {} (max {} requests per {}s)",
            peer_uid,
            RATE_LIMIT_MAX_REQUESTS,
            RATE_LIMIT_WINDOW.as_secs()
        ));
        write_response(stream, &response)?;
        return Ok(());
    }
    if let Err(err) = policy::authorize(&config.state_dir, peer_uid, &request.action) {
        let response = Response::err(err.to_string());
        write_response(stream, &response)?;
        return Ok(());
    }

    let response = match dispatch::dispatch(request, config, shutdown, port_publisher) {
        Ok(result) => Response::ok(result),
        Err(err) => Response::err(err.to_string()),
    };

    write_response(stream, &response)?;
    Ok(())
}

fn write_response(stream: &mut UnixStream, response: &Response) -> Result<()> {
    let payload = serde_json::to_vec(&response)?;
    stream.write_all(&payload)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

fn peer_uid(stream: &UnixStream) -> Result<u32> {
    let creds =
        getsockopt(stream, PeerCredentials).context("getsockopt(PeerCredentials) failed")?;
    Ok(creds.uid())
}

fn read_request_line(stream: &UnixStream) -> Result<Option<String>> {
    let mut request_raw = String::new();
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut limited_reader = reader.by_ref().take((MAX_REQUEST_BYTES + 1) as u64);
    let read = limited_reader
        .read_line(&mut request_raw)
        .context("failed to read request line")?;

    if read == 0 || request_raw.trim().is_empty() {
        return Ok(None);
    }
    if request_raw.len() > MAX_REQUEST_BYTES {
        bail!("request exceeds maximum size ({} bytes)", MAX_REQUEST_BYTES);
    }
    if !request_raw.ends_with('\n') {
        bail!("request must be newline-terminated");
    }
    Ok(Some(request_raw))
}

fn shutdown_requested(shutdown: &Arc<AtomicBool>) -> bool {
    shutdown.load(Ordering::SeqCst) || SIGNAL_SHUTDOWN.load(Ordering::SeqCst)
}

fn reconcile_published_ports(
    state_dir: &Path,
    port_publisher: &crate::network::publish::PortPublisher,
) -> Result<()> {
    let workspaces = crate::workspace::list_workspaces(state_dir, None)?;
    for workspace in workspaces {
        if workspace.status != WorkspaceStatus::Running || workspace.published_ports.is_empty() {
            continue;
        }

        let Some(runtime_pid) = workspace.runtime_pid else {
            tracing::warn!(
                "workspace {} is marked running without a runtime pid; skipping port republish",
                workspace.id
            );
            continue;
        };
        if !crate::workspace::session_process_matches(
            runtime_pid,
            workspace.runtime_starttime_ticks,
        ) {
            tracing::warn!(
                "workspace {} runtime pid {} is not alive; skipping port republish",
                workspace.id,
                runtime_pid
            );
            continue;
        }

        let Some(workspace_ip) = workspace.assigned_ip.as_deref() else {
            tracing::warn!(
                "workspace {} is running without an assigned IP; skipping port republish",
                workspace.id
            );
            continue;
        };

        port_publisher.reconcile_workspace_ports(
            &workspace.sandbox_id,
            &workspace.id,
            runtime_pid,
            workspace_ip,
            &workspace.published_ports,
        )?;
    }
    Ok(())
}

fn install_signal_handlers() -> Result<()> {
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

#[cfg(test)]
#[path = "../../tests/src/daemon/mod.rs"]
mod tests;
