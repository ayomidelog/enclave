//! Talking to the daemon from a command.
//!
//! A command is a thin client: it parses arguments, sends one or more requests,
//! and prints what came back. Two of those calls are worth naming.
//!
//! `send` is the plain call. `send_managed` is for the requests that mutate host
//! state: those need a running daemon, and starting one is a decision the operator
//! opts into, so the check is separate from the send rather than folded into it.
//!
//! Every mutating command prints the operation id the daemon reported, because
//! that id is the only thing that ties the command the operator typed to the
//! journal record, the log lines, and the phase timings for the work it caused.

use std::path::Path;

use anyhow::Result;

use super::daemon;

pub(crate) fn send(
    socket: &Path,
    action: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    crate::client::send_request(socket, action, params)
}

/// Send a request that changes host state, ensuring a daemon is available first.
pub(crate) fn send_managed(
    socket: &Path,
    action: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    daemon::ensure_daemon_running_for_action(socket, action)?;
    send(socket, action, params)
}

/// Report the operation id the daemon ran the last request as.
///
/// A command that issues several requests prints only the last, which is the one
/// that carried the user's intent.
pub(crate) fn print_operation_id() {
    if let Some(id) = crate::client::last_operation_id() {
        println!("operation {id}");
    }
}
