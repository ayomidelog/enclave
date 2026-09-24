use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::cli::{DaemonCommands, StartArgs};
use crate::config::FileConfig;
use crate::daemon::{run_daemon, DaemonConfig};
use crate::paths;
use crate::sandbox::SandboxListItem;
use crate::workspace::WorkspaceMetadata;

use super::send;

const MAX_DAEMON_LOG_BYTES: u64 = 10 * 1024 * 1024;
static AUTO_START_DEFAULTS: OnceLock<AutoStartDefaults> = OnceLock::new();
static EXPLICIT_DAEMON_START: OnceLock<bool> = OnceLock::new();

#[derive(Debug, Clone)]
struct AutoStartDefaults {
    state_dir: std::path::PathBuf,
    pid_file: std::path::PathBuf,
    debootstrap_binary: String,
    wait_secs: u64,
    workspace_apparmor_profile: Option<String>,
    workspace_selinux_label: Option<String>,
}

pub(crate) fn run_daemon_command(socket: &Path, command: DaemonCommands) -> Result<()> {
    match command {
        DaemonCommands::Run(args) => {
            if send(socket, "ping", json!({})).is_ok() {
                bail!(
                    "daemon already running on {}. Stop it first with `enclave daemon stop`.",
                    socket.display()
                );
            }
            run_daemon(DaemonConfig {
                socket_path: socket.to_path_buf(),
                state_dir: args.state_dir,
                pid_file: args.pid_file,
                debootstrap_binary: args.debootstrap_binary,
                workspace_apparmor_profile: args.workspace_apparmor_profile,
                workspace_selinux_label: args.workspace_selinux_label,
            })
        }
        DaemonCommands::Start(args) => start_daemon(socket, args),
        DaemonCommands::Stop => {
            match send(socket, "shutdown", json!({})) {
                Ok(_) => {
                    println!("daemon stop signal sent");
                }
                Err(err) => {
                    if crate::client::is_daemon_unreachable(&err) {
                        println!("daemon is not running");
                    } else {
                        return Err(err);
                    }
                }
            }
            Ok(())
        }
        DaemonCommands::Status => {
            let health = send(socket, "daemon.health", json!({}))
                .with_context(|| format!("daemon is not running on {}", socket.display()))?;
            let sandboxes_value = send(socket, "sandbox.list", json!({}))?;
            let workspaces_value = send(socket, "workspace.list", json!({}))?;
            let sandboxes: Vec<SandboxListItem> = serde_json::from_value(sandboxes_value)?;
            let workspaces: Vec<WorkspaceMetadata> = serde_json::from_value(workspaces_value)?;
            println!(
                "daemon running on {} ({} sandboxes, {} workspaces)",
                socket.display(),
                sandboxes.len(),
                workspaces.len()
            );
            report_daemon_activity(&health);
            report_deadline_overrides(&health);
            Ok(())
        }
    }
}

/// Print the deadlines an operator has overridden.
///
/// A wait that ends sooner or later than the documented default is otherwise
/// invisible: the operator sees the consequence (a start that failed, a stop that
/// took longer) without the cause. Only the overrides are printed, because the
/// defaults are the documented behaviour and printing seven of them on every
/// status call would bury the one that changed.
fn report_deadline_overrides(health: &Value) {
    let Some(deadlines) = health.get("deadlines").and_then(Value::as_array) else {
        return;
    };
    let overridden = deadlines
        .iter()
        .filter(|deadline| deadline.get("overridden") == Some(&Value::Bool(true)))
        .collect::<Vec<_>>();
    if overridden.is_empty() {
        return;
    }
    for deadline in overridden {
        let field = |name: &str| deadline.get(name).and_then(Value::as_str).unwrap_or("-");
        let value = deadline
            .get("value_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let default = deadline
            .get("default_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        println!(
            "deadline: {} is {value}ms (default {default}ms), set by {}",
            field("name"),
            field("variable"),
        );
    }
}

/// Say what the daemon is doing now and what it last did.
///
/// The operation ids are what tie a status report to the journal record, the log
/// lines, and the phase timings for the same operation, so they are the two
/// things worth printing beyond the counts.
fn report_daemon_activity(health: &Value) {
    if let Some(active) = health.get("active_operations").and_then(Value::as_array) {
        for operation in active {
            let field = |name: &str| operation.get(name).and_then(Value::as_str).unwrap_or("-");
            println!(
                "running: {} on {} for {}s (operation {})",
                field("action"),
                field("target"),
                operation
                    .get("elapsed_secs")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                field("operation_id"),
            );
        }
    }

    let Some(last) = health
        .get("last_operation")
        .filter(|value| !value.is_null())
    else {
        return;
    };
    let field = |name: &str| last.get(name).and_then(Value::as_str).unwrap_or("-");
    println!(
        "last: {} {} on {} ({}, updated {})",
        field("kind"),
        field("status"),
        field("target"),
        field("phase"),
        field("updated_at"),
    );
    if let Some(error) = last.get("error").and_then(Value::as_str) {
        println!("  error: {error}");
    }
}

fn start_daemon(socket: &Path, args: StartArgs) -> Result<()> {
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

        if let Some(status) = child.try_wait()? {
            bail!("daemon exited early with status {status}");
        }

        thread::sleep(Duration::from_millis(100));
    }

    bail!(
        "daemon did not become ready on {} within {}s",
        socket.display(),
        args.wait_secs
    )
}

pub(crate) fn configure_automatic_start_defaults(
    file_config: &FileConfig,
    explicit_daemon_start: bool,
) {
    let _ = AUTO_START_DEFAULTS.set(AutoStartDefaults {
        state_dir: file_config
            .state_dir
            .clone()
            .unwrap_or_else(paths::default_state_dir),
        pid_file: file_config
            .pid_file
            .clone()
            .unwrap_or_else(paths::default_pid_file),
        debootstrap_binary: file_config
            .debootstrap_binary
            .clone()
            .unwrap_or_else(|| "debootstrap".to_string()),
        wait_secs: file_config.wait_secs.unwrap_or(5),
        workspace_apparmor_profile: file_config.workspace_apparmor_profile.clone(),
        workspace_selinux_label: file_config.workspace_selinux_label.clone(),
    });
    let _ = EXPLICIT_DAEMON_START.set(explicit_daemon_start);
}

pub(crate) fn ensure_daemon_running_for_action(socket: &Path, action: &str) -> Result<()> {
    match send(socket, "ping", json!({})) {
        Ok(_) => return Ok(()),
        Err(err) => {
            // A daemon that answers the ping with an error is running; only an
            // unreachable one should be started here. Anything else — a policy
            // denial, a malformed response — has to reach the caller unchanged.
            if !crate::client::is_daemon_unreachable(&err) {
                return Err(err);
            }
        }
    }

    if destructive_action_requires_explicit_start(action)
        && !EXPLICIT_DAEMON_START.get().copied().unwrap_or(false)
    {
        bail!(
            "{} requires a running daemon at {}; start it explicitly with `enclave daemon start` or retry with `--start-daemon`",
            action,
            socket.display()
        );
    }

    let defaults = AUTO_START_DEFAULTS
        .get()
        .cloned()
        .unwrap_or_else(default_auto_start_defaults);
    let args = StartArgs {
        state_dir: defaults.state_dir,
        pid_file: defaults.pid_file,
        debootstrap_binary: defaults.debootstrap_binary,
        wait_secs: defaults.wait_secs,
        workspace_apparmor_profile: defaults.workspace_apparmor_profile,
        workspace_selinux_label: defaults.workspace_selinux_label,
    };
    start_daemon(socket, args)
}

pub(crate) fn ensure_daemon_running(socket: &Path) -> Result<()> {
    ensure_daemon_running_for_action(socket, "non_destructive")
}

fn destructive_action_requires_explicit_start(action: &str) -> bool {
    matches!(
        action,
        "sandbox.destroy"
            | "sandbox.remove"
            | "sandbox.wipe"
            | "workspace.destroy"
            | "workspace.remove"
            | "workspace.wipe"
            | "registry.repair"
            | "daemon.doctor.repair"
    )
}

fn default_auto_start_defaults() -> AutoStartDefaults {
    AutoStartDefaults {
        state_dir: paths::default_state_dir(),
        pid_file: paths::default_pid_file(),
        debootstrap_binary: "debootstrap".to_string(),
        wait_secs: 5,
        workspace_apparmor_profile: None,
        workspace_selinux_label: None,
    }
}

fn prepare_service_dir(path: &Path) -> Result<()> {
    if path.exists() {
        repair_root_owned_dir(path)?;
    }
    crate::fsutil::ensure_secure_dir(path)
}

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

#[cfg(test)]
#[path = "../../tests/src/commands/daemon.rs"]
mod tests;
