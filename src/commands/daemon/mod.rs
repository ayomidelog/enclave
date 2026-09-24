//! The daemon command group.
//!
//! `daemon run` is the foreground daemon process itself. The other commands
//! manage it from outside: `start` spawns a background daemon and waits for it
//! to answer, `stop` asks a running one to shut down, and `status` reports what
//! it is doing.
//!
//! The commands that change host state need a daemon, and starting one is a
//! decision the operator opts into. That policy, and the machinery for spawning
//! the background process, live in their own modules:
//!
//! - `start` spawns the daemon, prepares its directories, and rotates its log.
//! - `autostart` decides whether a command may start a daemon by itself.

mod autostart;
mod start;

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::cli::DaemonCommands;
use crate::daemon::{run_daemon, DaemonConfig};
use crate::sandbox::SandboxListItem;
use crate::workspace::WorkspaceMetadata;

use super::send;

pub(crate) use autostart::{
    configure_automatic_start_defaults, ensure_daemon_running, ensure_daemon_running_for_action,
};

pub(crate) fn run_daemon_command(socket: &Path, command: DaemonCommands) -> Result<()> {
    match command {
        DaemonCommands::Run(args) => {
            // A second daemon on the same socket would fight the first over the
            // state lock, so refuse before doing any work.
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
        DaemonCommands::Start(args) => start::start_daemon(socket, args),
        DaemonCommands::Stop => {
            match send(socket, "shutdown", json!({})) {
                Ok(_) => {
                    println!("daemon stop signal sent");
                }
                Err(err) => {
                    // "Nothing is listening" is the outcome the operator wanted,
                    // so it is a success here rather than an error. Any other
                    // failure is real and has to reach the caller.
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
/// defaults are the documented behaviour and printing every one of them on each
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
