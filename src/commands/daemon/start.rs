//! Spawning the background daemon and waiting for it to answer.
//!
//! `daemon run` is a foreground process; `daemon start` is this module: it
//! re-executes the binary as a detached child with the log as its output, then
//! polls the socket until the child answers or the wait expires. Polling the
//! socket rather than waiting on the child is what makes the wait meaningful: the
//! process existing is not the same as the daemon being ready to serve, and the
//! child's own exit is checked separately so an early failure is reported as
//! such rather than as a timeout.
//!
//! The directories the daemon writes to are prepared first. A state directory
//! owned by a non-root user is taken over rather than refused, because the
//! daemon needs root and the alternative is an operator who cannot start it
//! without knowing why.

use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::cli::StartArgs;

use super::super::send;

/// Rotate the daemon log once it passes this size.
const MAX_DAEMON_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// How often the wait re-checks whether the daemon answers.
const READY_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(super) fn start_daemon(socket: &Path, args: StartArgs) -> Result<()> {
    if send(socket, "ping", json!({})).is_ok() {
        println!("daemon already running on {}", socket.display());
        return Ok(());
    }

    prepare_service_dir(&args.state_dir)?;
    if let Some(runtime_dir) = socket.parent() {
        prepare_service_dir(runtime_dir)?;
    }
    if let Some(pid_dir) = args.pid_file.parent() {
        prepare_service_dir(pid_dir)?;
    }

    let exe = env::current_exe().context("failed to determine current executable path")?;
    let log_path = socket
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("daemon.log");
    rotate_daemon_log(&log_path, MAX_DAEMON_LOG_BYTES)?;
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open daemon log {}", log_path.display()))?;
    let log_file_err = log_file
        .try_clone()
        .with_context(|| format!("failed to clone daemon log {}", log_path.display()))?;
    let mut child = Command::new(exe)
        .arg("--socket")
        .arg(socket)
        .arg("daemon")
        .arg("run")
        .arg("--state-dir")
        .arg(args.state_dir)
        .arg("--pid-file")
        .arg(args.pid_file)
        .arg("--debootstrap-binary")
        .arg(args.debootstrap_binary)
        .args(
            args.workspace_apparmor_profile
                .as_ref()
                .map(|value| vec!["--workspace-apparmor-profile".to_string(), value.clone()])
                .unwrap_or_default(),
        )
        .args(
            args.workspace_selinux_label
                .as_ref()
                .map(|value| vec!["--workspace-selinux-label".to_string(), value.clone()])
                .unwrap_or_default(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err))
        .spawn()
        .context("failed to start daemon process")?;

    let started_at = Instant::now();
    let timeout = Duration::from_secs(args.wait_secs);

    while started_at.elapsed() < timeout {
        if send(socket, "ping", json!({})).is_ok() {
            println!(
                "daemon started (pid {}), logs: {}",
                child.id(),
                log_path.display()
            );
            return Ok(());
        }

        // A child that has already exited will never answer, so report why
        // instead of waiting out the whole timeout.
        if let Some(status) = child.try_wait()? {
            bail!("daemon exited early with status {status}");
        }

        thread::sleep(READY_POLL_INTERVAL);
    }

    bail!(
        "daemon did not become ready on {} within {}s",
        socket.display(),
        args.wait_secs
    )
}

/// Create a directory the daemon will write to, taking it over if needed.
fn prepare_service_dir(path: &Path) -> Result<()> {
    if path.exists() {
        repair_root_owned_dir(path)?;
    }
    crate::fsutil::ensure_secure_dir(path)
}

/// Give a directory to root when it exists and belongs to someone else.
///
/// A daemon that cannot write its own state directory is useless, and the fix is
/// unambiguous: the daemon runs as root, so root has to own the directory. The
/// check refuses anything that is not a real directory, so a symlink is never
/// followed into an unrelated path.
fn repair_root_owned_dir(path: &Path) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    if metadata.uid() == 0 || !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(());
    }

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("path contains interior NUL: {}", path.display()))?;
    let chown_rc = unsafe { libc::chown(c_path.as_ptr(), 0, 0) };
    if chown_rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to take ownership of {}", path.display()));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to tighten permissions on {}", path.display()))?;
    Ok(())
}

/// Rotate the daemon log to a single `.log.1` when it outgrows its budget.
///
/// One generation is enough: the log is diagnostic, and keeping more would cost
/// disk on a host that already has plenty of state to track.
fn rotate_daemon_log(log_path: &Path, max_bytes: u64) -> Result<()> {
    if !log_path.exists() {
        return Ok(());
    }
    let size = fs::metadata(log_path)
        .with_context(|| format!("failed to stat daemon log {}", log_path.display()))?
        .len();
    if size < max_bytes {
        return Ok(());
    }

    let rotated_path = log_path.with_extension("log.1");
    if rotated_path.exists() {
        fs::remove_file(&rotated_path)
            .with_context(|| format!("failed to remove {}", rotated_path.display()))?;
    }
    fs::rename(log_path, &rotated_path).with_context(|| {
        format!(
            "failed to rotate daemon log {} -> {}",
            log_path.display(),
            rotated_path.display()
        )
    })?;
    Ok(())
}
