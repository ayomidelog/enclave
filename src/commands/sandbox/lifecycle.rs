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
use crate::cli::SandboxResizeArgs;
use crate::sandbox::SandboxResizeReport;

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

/// Change a sandbox's resource limits.
///
/// Every field is optional, so raising memory does not disturb the disk budget. The
/// daemon answers with what the limits were, which is what makes the report a
/// comparison rather than a claim.
pub(crate) fn run_resize(socket: &Path, args: SandboxResizeArgs) -> Result<()> {
    let SandboxResizeArgs {
        sandbox,
        memory_mb,
        disk_mb,
        max_procs,
    } = args;
    tracing::info!("resizing sandbox '{}'...", sandbox);
    // Only the limits the operator named are sent. A field carrying `null` means
    // "clear this limit" to the daemon, so including an absent one would turn
    // "raise the disk budget" into "raise the disk budget and remove the memory
    // limit".
    let mut params = serde_json::Map::new();
    params.insert("sandbox".to_string(), json!(sandbox));
    if let Some(memory_mb) = memory_mb {
        params.insert("memory_mb".to_string(), json!(memory_mb));
    }
    if let Some(disk_mb) = disk_mb {
        params.insert("disk_mb".to_string(), json!(disk_mb));
    }
    if let Some(max_procs) = max_procs {
        params.insert("max_procs".to_string(), json!(max_procs));
    }
    let response = send_managed(socket, "sandbox.resize", serde_json::Value::Object(params))?;
    let report: SandboxResizeReport = serde_json::from_value(response)?;
    println!("resized sandbox '{}'", report.sandbox.id);
    let limits = &report.sandbox.limits;
    report_limit("memory", report.previous_memory_bytes, limits.memory_bytes);
    report_limit("disk budget", report.previous_disk_bytes, limits.disk_bytes);
    report_limit(
        "max processes",
        report.previous_max_processes,
        limits.max_processes,
    );
    print_operation_id();
    Ok(())
}

/// One limit, as it was and as it is now.
///
/// A limit that did not move is reported as unchanged rather than as a change to the
/// same number, so a resize that only touched one field does not look like it moved
/// three.
fn report_limit(label: &str, previous: Option<u64>, current: Option<u64>) {
    let render = |value: Option<u64>| match value {
        Some(bytes) if label == "memory" || label == "disk budget" => {
            format!("{} MiB", bytes / (1024 * 1024))
        }
        Some(value) => value.to_string(),
        None => "unlimited".to_string(),
    };
    if previous == current {
        println!("  {label}: {} (unchanged)", render(current));
    } else {
        println!("  {label}: {} -> {}", render(previous), render(current));
    }
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
