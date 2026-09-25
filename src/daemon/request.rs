//! Handling one client request.
//!
//! A request is one operation: it gets an operation id, is registered so shutdown
//! can name it, is rate limited and authorized, is dispatched, and is answered.
//! The id is what ties the response, the logs, and the lifecycle journal to the
//! same operation.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};

use crate::error::ErrorCode;
use crate::operation;
use crate::policy;
use crate::protocol::{Request, Response};

use super::services;
use super::{dispatch, DaemonConfig};

use super::{RATE_LIMIT_MAX_REQUESTS, RATE_LIMIT_WINDOW};

const MAX_REQUEST_BYTES: usize = 64 * 1024;

pub(super) fn handle_client(
    mut stream: UnixStream,
    config: &DaemonConfig,
    shutdown: &Arc<AtomicBool>,
    services: &services::DaemonServices,
) -> Result<()> {
    crate::perf::record_request();
    let _request_total = crate::perf::Timer::new("daemon.request");
    let request_raw = match read_request_line(&stream) {
        Ok(Some(request_raw)) => request_raw,
        Ok(None) => return Ok(()),
        Err(err) => {
            // The request never parsed, so it has no id of its own. A fresh one
            // still lets the caller find the failure in the logs.
            let response = Response::err_code(
                ErrorCode::InvalidRequest,
                err.to_string(),
                operation::new_id(),
            );
            write_response(&mut stream, &response)?;
            return Ok(());
        }
    };

    let request: Request = match serde_json::from_str(&request_raw) {
        Ok(request) => request,
        Err(err) => {
            let response = Response::err_code(
                ErrorCode::InvalidRequest,
                format!("invalid request payload: {err}"),
                operation::new_id(),
            );
            write_response(&mut stream, &response)?;
            return Ok(());
        }
    };

    // One request is one operation. The id is the trace id the caller is told
    // about, the id the lifecycle journal records, and the id every log line and
    // phase timing for this request carries.
    let operation_id = request
        .operation_id
        .as_deref()
        .filter(|id| operation::is_valid_id(id))
        .map(str::to_string)
        .unwrap_or_else(operation::new_id);
    operation::set_current(Some(operation_id.clone()));
    let _clear = ClearOperationOnDrop;

    // A lifecycle operation is worth one line at the default level: it is how an
    // operator ties a command to the journal record and the phase timings for the
    // same operation. A read-only request stays quiet, so the log is not doubled
    // for status polling.
    // A lifecycle operation is registered so shutdown can name what it
    // interrupted, and so a report of "nothing was running" is evidence rather
    // than silence. Read-only requests are not registered: they are short, and
    // registering them would make shutdown wait on status polling.
    let _active = dispatch::action_is_lifecycle(&request.action).then(|| {
        services.active_operations.begin(
            &operation_id,
            &request.action,
            &operation_target(&request.params),
        )
    });

    if dispatch::action_is_lifecycle(&request.action) {
        tracing::info!(
            operation_id = %operation_id,
            action = %request.action,
            "lifecycle operation started"
        );
    }

    let peer_uid = peer_uid(&stream).context("failed to resolve peer uid")?;
    if !services.rate_limiter.allow(peer_uid) {
        let response = Response::err_code(
            ErrorCode::RateLimited,
            format!(
                "rate limit exceeded for uid {} (max {} requests per {}s)",
                peer_uid,
                RATE_LIMIT_MAX_REQUESTS,
                RATE_LIMIT_WINDOW.as_secs()
            ),
            operation_id,
        );
        write_response(&mut stream, &response)?;
        return Ok(());
    }
    if let Err(err) = policy::authorize(&config.state_dir, peer_uid, &request.action) {
        let response = Response::err_code(ErrorCode::PolicyDenied, err.to_string(), operation_id);
        write_response(&mut stream, &response)?;
        return Ok(());
    }

    let _dispatch = crate::perf::Timer::new("daemon.dispatch");
    let response = match dispatch::dispatch(request, config, shutdown, services, Some(&stream)) {
        Ok(result) => Response::ok(result, operation_id),
        Err(err) => {
            // A lifecycle failure is the one place the id is worth repeating at
            // the default log level: it is how an operator finds the journal
            // record for the operation that failed.
            tracing::error!(error = %format!("{err:#}"), "request failed");
            Response::err_code(crate::error::code_of(&err), err.to_string(), operation_id)
        }
    };
    drop(_dispatch);

    write_response(&mut stream, &response)?;
    Ok(())
}

/// A short human label for what a request acts on, for shutdown reporting.
///
/// The sandbox and workspace names are the two things an operator needs to find
/// the affected state, and both are optional, so whatever the request carries is
/// used and the rest is left out rather than invented.
fn operation_target(params: &serde_json::Value) -> String {
    let field = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| params.get(*name).and_then(serde_json::Value::as_str))
    };
    let sandbox = field(&["sandbox", "sandbox_id", "name"]);
    let workspace = field(&["workspace", "workspace_id"]);
    match (sandbox, workspace) {
        (Some(sandbox), Some(workspace)) => format!("{sandbox}/{workspace}"),
        (Some(sandbox), None) => sandbox.to_string(),
        (None, Some(workspace)) => workspace.to_string(),
        (None, None) => "-".to_string(),
    }
}

/// Clear the thread-local operation id when the request that set it finishes.
///
/// A worker thread outlives the request, and a later request that does not set an
/// id would otherwise inherit this one.
struct ClearOperationOnDrop;

impl Drop for ClearOperationOnDrop {
    fn drop(&mut self) {
        operation::set_current(None);
    }
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
