//! Running commands in a workspace through helpers that stay alive.
//!
//! A helper holds the workspace's namespaces open and executes commands in them, so
//! a command does not pay for entering those namespaces on every call. Each runtime
//! has a helper pool: idle helpers serve sequential work, while concurrent commands
//! each get their own helper. Every helper remains a process-group leader so
//! cancellation has a verified host identity to signal.
//!
//! The helper submodule starts a process and waits for its socket, the transport
//! submodule sends one command over that socket, and this module owns the pools.

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

/// The runtime a pool serves: its pid and the start time that makes the pid
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

/// The helper pool for one runtime.
///
/// The daemon's exec worker pool bounds the total number of active commands. This
/// pool reuses idle helpers for sequential work and starts another whenever a
/// command arrives while every cached helper is busy. It does not impose a smaller
/// per-workspace limit unless `ENCLAVE_EXEC_PER_WORKSPACE` is configured.
struct HelperPool {
    state: Mutex<HelperPoolState>,
}

#[derive(Default)]
struct HelperPoolState {
    idle: Vec<PersistentHelper>,
    active: Vec<HelperIdentity>,
    starting: usize,
    closed: bool,
}

#[derive(Debug, Clone)]
struct HelperIdentity {
    pid: u32,
    starttime_ticks: u64,
    socket: PathBuf,
}

impl PersistentHelper {
    fn identity(&self) -> HelperIdentity {
        HelperIdentity {
            pid: self.child.id(),
            starttime_ticks: self.starttime_ticks,
            socket: self.socket.clone(),
        }
    }
}

static HELPERS: OnceLock<Mutex<HashMap<HelperKey, Arc<HelperPool>>>> = OnceLock::new();

/// The shared inputs for one command run through a helper.
struct HelperCommand<'a> {
    workspace: &'a WorkspaceMetadata,
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    cwd: &'a str,
    command: &'a [String],
    client_stream: Option<&'a UnixStream>,
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
    let pool = helper_pool(key)?;
    let concurrency_limit = crate::workspace::max_concurrent_commands_per_workspace();
    let helper = pool.acquire(
        workspace,
        runtime_pid,
        runtime_starttime_ticks,
        concurrency_limit,
    )?;
    run_on_helper(
        &pool,
        helper,
        HelperCommand {
            workspace,
            runtime_pid,
            runtime_starttime_ticks,
            cwd,
            command,
            client_stream,
        },
    )
}

fn helper_pool(key: HelperKey) -> Result<Arc<HelperPool>> {
    let helpers = HELPERS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = helpers
        .lock()
        .map_err(|_| anyhow::anyhow!("persistent helper cache lock poisoned"))?;
    Ok(Arc::clone(guard.entry(key).or_insert_with(|| {
        Arc::new(HelperPool {
            state: Mutex::new(HelperPoolState::default()),
        })
    })))
}

impl HelperPool {
    /// Reserve a slot, preferring an already-running helper.
    ///
    /// A direct caller over an explicitly configured workspace limit is rejected
    /// rather than parked here. Daemon requests are bounded by `ExecQueue` before
    /// they reach this layer, so waiting here would only turn an admission bug into
    /// a stuck request. Without a configured limit, the daemon's exec worker pool
    /// is the capacity bound and this method never waits.
    fn acquire(
        &self,
        workspace: &WorkspaceMetadata,
        runtime_pid: u32,
        runtime_starttime_ticks: u64,
        limit: Option<usize>,
    ) -> Result<PersistentHelper> {
        let mut state = self.lock()?;
        if state.closed {
            return Err(stop_conflict(workspace));
        }
        while let Some(mut helper) = state.idle.pop() {
            if helper_is_alive(&mut helper) {
                state.active.push(helper.identity());
                return Ok(helper);
            }
            drop(state);
            kill_helper_group(&mut helper);
            state = self.lock()?;
            if state.closed {
                return Err(stop_conflict(workspace));
            }
        }
        if let Some(limit) =
            limit.filter(|limit| state.active.len().saturating_add(state.starting) >= *limit)
        {
            return Err(crate::error::coded(
                crate::error::ErrorCode::Conflict,
                format!(
                    "workspace '{}' already has {} commands running; wait for one to finish or cancel them",
                    workspace.id, limit
                ),
            ));
        }
        state.starting += 1;
        drop(state);

        let started = start_helper(workspace, runtime_pid, runtime_starttime_ticks);
        let mut state = self.lock()?;
        state.starting = state.starting.saturating_sub(1);
        match started {
            Ok(mut helper) => {
                if state.closed {
                    drop(state);
                    kill_helper_group(&mut helper);
                    return Err(stop_conflict(workspace));
                }
                state.active.push(helper.identity());
                Ok(helper)
            }
            Err(error) => Err(error),
        }
    }

    /// Return a healthy helper to the idle set, or kill one that cannot be reused.
    fn release(&self, helper: PersistentHelper, reusable: bool) {
        let mut helper = Some(helper);
        let keep = {
            // Recovering a poisoned guard is safe here: the state is a collection
            // of identities and idle children, not a partially updated invariant.
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let identity = helper.as_ref().map(PersistentHelper::identity);
            if let Some(identity) = identity {
                state.active.retain(|active| {
                    active.pid != identity.pid || active.starttime_ticks != identity.starttime_ticks
                });
            }
            let keep = reusable
                && !state.closed
                && state.idle.len() < crate::workspace::IDLE_HELPER_LIMIT
                && helper.as_mut().is_some_and(helper_is_alive);
            if keep {
                if let Some(helper) = helper.take() {
                    state.idle.push(helper);
                }
            }
            keep
        };
        if !keep {
            if let Some(mut helper) = helper {
                kill_helper_group(&mut helper);
            }
        }
    }

    fn close(&self) {
        let (idle, active) = if let Ok(mut state) = self.state.lock() {
            state.closed = true;
            (
                std::mem::take(&mut state.idle),
                std::mem::take(&mut state.active),
            )
        } else {
            (Vec::new(), Vec::new())
        };
        for mut helper in idle {
            kill_helper_group(&mut helper);
        }
        for helper in active {
            let _ = crate::workspace::session::signal_process_group(
                helper.pid,
                Some(helper.starttime_ticks),
                helper.pid as i32,
                libc::SIGKILL,
            );
            let _ = fs::remove_file(&helper.socket);
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HelperPoolState>> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("persistent helper pool lock poisoned"))
    }
}

fn stop_conflict(workspace: &WorkspaceMetadata) -> anyhow::Error {
    crate::error::coded(
        crate::error::ErrorCode::Conflict,
        format!(
            "workspace '{}' is stopping; wait for the stop to finish before running another command",
            workspace.id
        ),
    )
}

/// Send one command through a helper and account for what the outcome means.
fn run_on_helper(
    pool: &HelperPool,
    mut helper: PersistentHelper,
    request: HelperCommand<'_>,
) -> Result<PersistentCommandOutput> {
    let HelperCommand {
        workspace,
        runtime_pid,
        runtime_starttime_ticks,
        cwd,
        command,
        client_stream,
    } = request;
    let workspace_path = std::path::Path::new(&workspace.workspace_path);
    // Publish this command's helper before the request is sent. The helper is the
    // only addressable handle on work inside the workspace's pid namespace, and a
    // per-command file lets a second concurrent command publish its own handle
    // without overwriting this one. The daemon writes it because the helper's mount
    // namespace does not expose the host state directory at its host path.
    let marker = match crate::workspace::cancel::publish_command_marker(
        workspace_path,
        helper.child.id(),
        helper.starttime_ticks,
    ) {
        Ok(marker) => Some(marker),
        Err(error) => {
            tracing::warn!(
                "failed to publish a cancellation marker for workspace {}: {error:#}",
                workspace.id
            );
            None
        }
    };
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
    let mut helper_reusable = result.as_ref().err().is_none_or(|error| {
        !(transport::HelperTimeout::is_in(error)
            || transport::HelperClosed::is_in(error)
            || transport::ClientDisconnected::is_in(error))
    });
    if let Some(marker) = marker {
        match crate::workspace::cancel::finish_command_marker(&marker) {
            Ok(true) => {}
            Ok(false) => {
                // Cancellation removed the marker and signalled this helper. Even if
                // the command raced to a successful result, the helper is not safe to
                // reuse and its process group may already be gone.
                helper_reusable = false;
            }
            Err(error) => {
                tracing::warn!(
                    "failed to remove a cancellation marker for workspace {}: {error:#}",
                    workspace.id
                );
                helper_reusable = false;
            }
        }
    }
    pool.release(helper, helper_reusable);
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

fn helper_is_alive(helper: &mut PersistentHelper) -> bool {
    helper.child.try_wait().is_ok_and(|status| status.is_none())
}

pub(crate) fn invalidate(pid: u32, starttime_ticks: u64) {
    let Some(helpers) = HELPERS.get() else {
        return;
    };
    let pool = if let Ok(mut guard) = helpers.lock() {
        guard.remove(&HelperKey {
            pid,
            starttime_ticks,
        })
    } else {
        None
    };
    if let Some(pool) = pool {
        pool.close();
    }
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

pub(crate) fn output_to_string(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
#[path = "../../../../tests/src/workspace/session/persistent.rs"]
mod tests;
