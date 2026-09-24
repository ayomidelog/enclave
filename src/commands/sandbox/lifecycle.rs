//! Changing a sandbox's lifecycle state.
//!
//! Every command here sends one request and prints what the daemon reported. The
//! work itself lives in the daemon, which owns the registry and the host
//! resources; the client's job is to name the sandbox, carry the force flag
//! through, and print the operation id and the state change so the run can be
//! traced.
//!
//! Destroy prints the host resources a force teardown could not release. A force
//! destroy removes the registry record either way, so those lines are the only
//! remaining description of what is still running.

use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::cli::DestroyArgs;

use super::super::{
    print_operation_id, print_state_transition, report_retained_resources, send_managed,
};

/// Run one state-changing request and report the transition it made.
fn run_sandbox_state_change(socket: &Path, action: &str, sandbox: &str, verb: &str) -> Result<()> {
    tracing::info!("{verb} sandbox '{sandbox}'...");
    let response = send_managed(socket, action, json!({ "sandbox": sandbox }))?;
    println!("{verb} sandbox '{sandbox}'");
    print_state_transition(&response);
    print_operation_id();
    Ok(())
}

pub(crate) fn run_start(socket: &Path, sandbox: &str) -> Result<()> {
    run_sandbox_state_change(socket, "sandbox.start", sandbox, "started")
}

pub(crate) fn run_stop(socket: &Path, sandbox: &str) -> Result<()> {
    run_sandbox_state_change(socket, "sandbox.stop", sandbox, "stopped")
}

pub(crate) fn run_pause(socket: &Path, sandbox: &str) -> Result<()> {
    run_sandbox_state_change(socket, "sandbox.pause", sandbox, "paused")
}

pub(crate) fn run_resume(socket: &Path, sandbox: &str) -> Result<()> {
    run_sandbox_state_change(socket, "sandbox.resume", sandbox, "resumed")
}

pub(crate) fn run_destroy(socket: &Path, args: DestroyArgs) -> Result<()> {
    let DestroyArgs { sandbox, force } = args;
    tracing::info!(
        "destroying sandbox '{}'{}...",
        sandbox,
        if force { " (force)" } else { "" }
    );
    let response = send_managed(
        socket,
        "sandbox.destroy",
        json!({ "sandbox": sandbox, "force": force }),
    )?;
    let report: crate::sandbox::SandboxDestroyReport = serde_json::from_value(response.clone())?;
    println!("destroyed sandbox '{}'", report.sandbox_id);
    print_state_transition(&response);
    print_operation_id();
    report_retained_resources(
        report
            .retained
            .iter()
            .map(|item| format!("{}: {}", item.resource, item.detail)),
    );
    Ok(())
}
