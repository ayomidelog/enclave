//! Whether a command may start a daemon by itself.
//!
//! Most commands just need a daemon, and starting one when none is running is the
//! behaviour an operator expects. The destructive ones are different: if a daemon
//! is not running, the state on disk is whatever the last daemon left, and a
//! destroy that starts a fresh daemon to delete it is not obviously what was
//! meant. Those actions therefore require the daemon to be running already, or
//! an explicit `--start-daemon` for that one invocation.
//!
//! The config file supplies the arguments a daemon is started with, so they are
//! resolved once at startup and remembered here. A caller that never ran the CLI
//! entry point still gets usable defaults.

use std::path::Path;
use std::sync::OnceLock;

use anyhow::{bail, Result};
use serde_json::json;

use crate::cli::StartArgs;
use crate::config::FileConfig;
use crate::paths;

use super::super::send;
use super::start::start_daemon;

/// The arguments an automatically started daemon is given.
#[derive(Debug, Clone)]
struct AutoStartDefaults {
    state_dir: std::path::PathBuf,
    pid_file: std::path::PathBuf,
    debootstrap_binary: String,
    wait_secs: u64,
    workspace_apparmor_profile: Option<String>,
    workspace_selinux_label: Option<String>,
}

/// Set once by the CLI entry point from the config file.
static AUTO_START_DEFAULTS: OnceLock<AutoStartDefaults> = OnceLock::new();
/// Set once by the CLI entry point from `--start-daemon`.
static EXPLICIT_DAEMON_START: OnceLock<bool> = OnceLock::new();

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

/// Ensure a daemon is running for `action`, starting one when that is allowed.
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

/// Ensure a daemon is running for a command that does not destroy state.
pub(crate) fn ensure_daemon_running(socket: &Path) -> Result<()> {
    ensure_daemon_running_for_action(socket, "non_destructive")
}

/// Whether starting a daemon for this action has to be explicit.
///
/// The list is the actions that delete state: a destroy, a remove, a wipe, and
/// the two repairs. A destroy of a workspace is included because the record it
/// removes is the only description of the host resources it owned.
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

#[cfg(test)]
#[path = "../../../tests/src/commands/daemon.rs"]
mod tests;
