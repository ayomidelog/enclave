//! Running a command in a workspace through a helper that stays alive.
//!
//! One helper is kept per live runtime. It holds the workspace's namespaces open
//! and executes commands in them, so a command does not pay for opening and
//! re-entering the namespaces on every call. The helper is keyed by the runtime's
//! pid and start time, so a recycled pid cannot address a dead runtime's helper.
//!
//! The helper submodule starts the process and waits for its socket, the
//! transport submodule sends one command over that socket, and this module owns
//! the cache that ties the two together.

mod helper;
mod transport;

use std::collections::HashMap;
use std::fs;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{bail, Context, Result};

use super::process_matches;
use crate::workspace::types::WorkspaceMetadata;

use helper::start_helper;
use transport::send_command;

pub(crate) use transport::PersistentCommandOutput;

pub(crate) const MAX_HELPER_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// The runtime a helper serves: its pid and the start time that makes the pid
/// unambiguous across recycling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct HelperKey {
    pid: u32,
    starttime_ticks: u64,
}

/// One running helper. The socket and token are what the transport needs, and the
/// child is kept so its exit can be noticed and its socket removed.
pub(super) struct PersistentHelper {
    pub(super) socket: PathBuf,
    pub(super) auth_token: String,
    pub(super) starttime_ticks: u64,
    pub(super) child: Child,
}

static HELPERS: OnceLock<Mutex<HashMap<HelperKey, Arc<Mutex<PersistentHelper>>>>> = OnceLock::new();

/// The shared inputs for one command run through a cached helper.
struct HelperCommand<'a> {
    helper: Arc<Mutex<PersistentHelper>>,
    workspace: &'a WorkspaceMetadata,
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    cwd: &'a str,
    command: &'a [String],
    client_stream: Option<&'a UnixStream>,
    key: HelperKey,
    helpers: &'a Mutex<HashMap<HelperKey, Arc<Mutex<PersistentHelper>>>>,
}

pub(crate) fn execute_persistent_command(
    workspace: &WorkspaceMetadata,
    cwd: &str,
    command: &[String],
    client_stream: Option<&UnixStream>,
) -> Result<PersistentCommandOutput> {
    let runtime_pid = workspace
        .runtime_pid
        .context("workspace has no runtime pid")?;
    let runtime_starttime_ticks = workspace
        .runtime_starttime_ticks
        .context("workspace has no runtime start time")?;
    if !process_matches(runtime_pid, Some(runtime_starttime_ticks)) {
        bail!(
            "workspace runtime pid {} is not alive or no longer matches expected start time",
            runtime_pid
        );
    }

    let key = HelperKey {
        pid: runtime_pid,
        starttime_ticks: runtime_starttime_ticks,
    };
    let helpers = HELPERS.get_or_init(|| Mutex::new(HashMap::new()));
    // Check the cache under its lock, then start outside it. The helper's start
    // can take up to ENCLAVE_HELPER_START_MS, and holding the cache lock through
    // that would serialize every workspace's first command behind one slow start.
    // Unique socket names make concurrent starts safe: whichever helper is
    // inserted wins, and every losing helper is killed before it can become an
    // orphan.
    let helper = {
        let cached = cached_helper(helpers, key)?;
        if let Some(cached) = cached {
            if helper_is_alive(&cached)? {
                cached
            } else {
                discard_cached_helper(&key, &cached, workspace);
                install_started_helper(
                    workspace,
                    runtime_pid,
                    runtime_starttime_ticks,
                    key,
                    helpers,
                )?
            }
        } else {
            install_started_helper(
                workspace,
                runtime_pid,
                runtime_starttime_ticks,
                key,
                helpers,
            )?
        }
    };
    run_on_helper(HelperCommand {
        helper,
        workspace,
        runtime_pid,
        runtime_starttime_ticks,
        cwd,
        command,
        client_stream,
        key,
        helpers,
    })
}

/// Start a helper and publish it only if no live helper won the race.
fn install_started_helper(
    workspace: &WorkspaceMetadata,
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    key: HelperKey,
    helpers: &Mutex<HashMap<HelperKey, Arc<Mutex<PersistentHelper>>>>,
) -> Result<Arc<Mutex<PersistentHelper>>> {
    let started = start_helper(workspace, runtime_pid, runtime_starttime_ticks)?;
    let candidate = Arc::new(Mutex::new(started));
    loop {
        if let Some(current) = cached_helper(helpers, key)? {
            // Never hold the cache lock while waiting for a helper mutex. A
            // command holds that mutex for its whole run, so doing so would
            // serialize every workspace's helper lookup behind it.
            if helper_is_alive(&current)? {
                if let Ok(mut losing) = candidate.lock() {
                    kill_helper_group(&mut losing);
                }
                return Ok(current);
            }
            discard_cached_helper(&key, &current, workspace);
            continue;
        }
        if install_if_vacant(helpers, key, &candidate)? {
            return Ok(candidate);
        }
    }
}

fn cached_helper(
    helpers: &Mutex<HashMap<HelperKey, Arc<Mutex<PersistentHelper>>>>,
    key: HelperKey,
) -> Result<Option<Arc<Mutex<PersistentHelper>>>> {
    let guard = helpers
        .lock()
        .map_err(|_| anyhow::anyhow!("persistent helper cache lock poisoned"))?;
    Ok(guard.get(&key).cloned())
}

fn install_if_vacant(
    helpers: &Mutex<HashMap<HelperKey, Arc<Mutex<PersistentHelper>>>>,
    key: HelperKey,
    candidate: &Arc<Mutex<PersistentHelper>>,
) -> Result<bool> {
    let mut guard = helpers
        .lock()
        .map_err(|_| anyhow::anyhow!("persistent helper cache lock poisoned"))?;
    if guard.contains_key(&key) {
        return Ok(false);
    }
    guard.insert(key, Arc::clone(candidate));
    Ok(true)
}

/// Send one command through `helper` and account for what the outcome means.
fn run_on_helper(request: HelperCommand<'_>) -> Result<PersistentCommandOutput> {
    let HelperCommand {
        helper: helper_handle,
        workspace,
        runtime_pid,
        runtime_starttime_ticks,
        cwd,
        command,
        client_stream,
        key,
        helpers,
    } = request;
    let mut helper = helper_handle
        .lock()
        .map_err(|_| anyhow::anyhow!("persistent helper lock poisoned"))?;
    // Publish this workspace as busy, with the helper's pid, before the command is
    // sent. The helper holds this workspace's command mutex from here until the
    // command exits, and a cancel that arrives in that window can stop it by
    // signalling the helper - which is the only addressable handle on work running
    // inside the workspace's pid namespace. The daemon writes this file, not the
    // helper: the helper's mount namespace does not expose the host state directory
    // at its host path, so a file it wrote would land somewhere the daemon cannot
    // read.
    let busy_path =
        crate::workspace::cancel_file_path(std::path::Path::new(&workspace.workspace_path));
    let _ = publish_busy_marker(&busy_path, helper.child.id(), helper.starttime_ticks);
    let mut result = send_command(
        &mut helper,
        runtime_pid,
        runtime_starttime_ticks,
        workspace,
        cwd,
        command,
        client_stream,
    );
    if let Ok(output) = result.as_mut() {
        output.helper_pid = Some(helper.child.id());
    }
    let _ = fs::remove_file(&busy_path);
    let helper_failed = result.as_ref().err().is_some_and(|error| {
        transport::HelperTimeout::is_in(error)
            || transport::HelperClosed::is_in(error)
            || transport::ClientDisconnected::is_in(error)
    });
    if helper_failed {
        // The helper stopped answering or closed without a result. It is holding
        // this workspace's command mutex, so leaving it in the cache would make
        // every later command queue behind a socket that will never speak again.
        kill_helper_group(&mut helper);
    }
    drop(helper);
    if helper_failed {
        evict_helper(key, helpers, &helper_handle);
    }
    if result
        .as_ref()
        .err()
        .is_some_and(transport::ClientDisconnected::is_in)
    {
        return Err(crate::error::coded(
            crate::error::ErrorCode::Conflict,
            format!(
                "client disconnected; cancelled the command in workspace {} in sandbox {}",
                workspace.id, workspace.sandbox_id
            ),
        ));
    }
    result
}

fn helper_is_alive(helper: &Arc<Mutex<PersistentHelper>>) -> Result<bool> {
    let mut helper = helper
        .lock()
        .map_err(|_| anyhow::anyhow!("persistent helper lock poisoned"))?;
    Ok(helper.child.try_wait()?.is_none())
}

/// Remove one dead helper, but only if it is still the cache entry for `key`.
fn discard_cached_helper(
    key: &HelperKey,
    expected: &Arc<Mutex<PersistentHelper>>,
    workspace: &WorkspaceMetadata,
) {
    let Some(helpers) = HELPERS.get() else {
        return;
    };
    let removed = helpers.lock().is_ok_and(|mut guard| {
        if guard
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, expected))
        {
            guard.remove(key);
            true
        } else {
            false
        }
    });
    if !removed {
        return;
    }
    if let Ok(mut helper) = expected.lock() {
        kill_helper_group(&mut helper);
    }
    let _ = fs::remove_file(crate::workspace::cancel_file_path(std::path::Path::new(
        &workspace.workspace_path,
    )));
}

pub(crate) fn invalidate(pid: u32, starttime_ticks: u64) {
    let Some(helpers) = HELPERS.get() else {
        return;
    };
    let removed = if let Ok(mut guard) = helpers.lock() {
        guard.remove(&HelperKey {
            pid,
            starttime_ticks,
        })
    } else {
        None
    };
    if let Some(helper) = removed {
        if let Ok(mut helper) = helper.lock() {
            kill_helper_group(&mut helper);
        }
    }
}

/// Record that this workspace is running a command, and which helper owns it.
///
/// Written atomically so a cancel never reads a half-written id.
fn publish_busy_marker(
    path: &std::path::Path,
    helper_pid: u32,
    starttime_ticks: u64,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("tmp");
    fs::write(&temp, format!("{helper_pid} {starttime_ticks}\n"))?;
    fs::rename(&temp, path)?;
    Ok(())
}

/// Kill a helper and every command descendant that shares its process group.
fn kill_helper_group(helper: &mut PersistentHelper) {
    let pid = helper.child.id();
    let _ = crate::workspace::session::signal_process_group(
        pid,
        Some(helper.starttime_ticks),
        pid as i32,
        libc::SIGKILL,
    );
    let _ = helper.child.kill();
    let _ = helper.child.wait();
    let _ = fs::remove_file(&helper.socket);
}

/// Remove a dead helper from the cache, but only if it is still the same one.
fn evict_helper(
    key: HelperKey,
    helpers: &Mutex<HashMap<HelperKey, Arc<Mutex<PersistentHelper>>>>,
    expected: &Arc<Mutex<PersistentHelper>>,
) {
    if let Ok(mut guard) = helpers.lock() {
        if guard
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, expected))
        {
            guard.remove(&key);
        }
    }
}

pub(crate) fn output_to_string(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
#[path = "../../../../tests/src/workspace/session/persistent.rs"]
mod tests;
