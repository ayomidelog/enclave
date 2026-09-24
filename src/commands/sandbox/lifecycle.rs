//! Changing a sandbox's lifecycle state.
//!
//! Every command here sends one request and prints what the daemon reported. The
//! work itself lives in the daemon, which owns the registry and the host
//! resources; the client's job is to name the sandbox, carry the force flag
//! through, and print the operation id so the run can be traced.
//!
//! Destroy prints the host resources a force teardown could not release. A force
//! destroy removes the registry record either way, so those lines are the only
//! remaining description of what is still running.

use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::cli::DestroyArgs;

use super::super::{print_operation_id, report_retained_resources, send_managed};

pub(crate) fn run_start(socket: &Path, sandbox: &str) -> Result<()> {
    tracing::info!("starting sandbox '{}'...", sandbox);
    send_managed(socket, "sandbox.start", json!({ "sandbox": sandbox }))?;
    println!("started sandbox '{}'", sandbox);
    print_operation_id();
    Ok(())
}

pub(crate) fn run_stop(socket: &Path, sandbox: &str) -> Result<()> {
    tracing::info!("stopping sandbox '{}'...", sandbox);
    send_managed(socket, "sandbox.stop", json!({ "sandbox": sandbox }))?;
    println!("stopped sandbox '{}'", sandbox);
    print_operation_id();
    Ok(())
}

pub(crate) fn run_pause(socket: &Path, sandbox: &str) -> Result<()> {
    tracing::info!("pausing sandbox '{}'...", sandbox);
    send_managed(socket, "sandbox.pause", json!({ "sandbox": sandbox }))?;
    println!("paused sandbox '{}'", sandbox);
    print_operation_id();
    Ok(())
}

pub(crate) fn run_resume(socket: &Path, sandbox: &str) -> Result<()> {
    tracing::info!("resuming sandbox '{}'...", sandbox);
    send_managed(socket, "sandbox.resume", json!({ "sandbox": sandbox }))?;
    println!("resumed sandbox '{}'", sandbox);
    print_operation_id();
    Ok(())
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
    let report: crate::sandbox::SandboxDestroyReport = serde_json::from_value(response)?;
    println!("destroyed sandbox '{}'", report.sandbox_id);
    print_operation_id();
    report_retained_resources(
        report
            .retained
            .iter()
            .map(|item| format!("{}: {}", item.resource, item.detail)),
    );
    Ok(())
}
