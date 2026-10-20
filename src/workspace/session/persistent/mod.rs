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
    pub(super) child: Child,
}

static HELPERS: OnceLock<Mutex<HashMap<HelperKey, Arc<Mutex<PersistentHelper>>>>> = OnceLock::new();

pub(crate) fn execute_persistent_command(
    workspace: &WorkspaceMetadata,
    cwd: &str,
    command: &[String],
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
    // Two passes at most: the first may find a cached helper that died (a cancel
    // killed it, or it exited on its own), evict it, and then start a fresh one.
    // Without the second pass a cancelled workspace stayed broken until the daemon
    // restarted, because every later command found the same dead helper.
    let helper = {
        let mut guard = helpers
            .lock()
            .map_err(|_| anyhow::anyhow!("persistent helper cache lock poisoned"))?;
        match guard.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => Arc::clone(entry.get()),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let helper = Arc::new(Mutex::new(start_helper(
                    workspace,
                    runtime_pid,
                    runtime_starttime_ticks,
                )?));
                entry.insert(Arc::clone(&helper));
                helper
            }
        }
    };
    let mut alive = helper
        .lock()
        .map_err(|_| anyhow::anyhow!("persistent helper lock poisoned"))?;
    if alive.child.try_wait()?.is_some() {
        // A helper killed by a cancel, or one that died on its own, must leave the
        // cache here. Otherwise the next command finds the corpse and fails, and
        // every command after it does the same: a cancelled workspace would never
        // recover without restarting the daemon.
        drop(alive);
        if let Ok(mut guard) = helpers.lock() {
            guard.remove(&key);
        }
        let _ = fs::remove_file(crate::workspace::cancel_file_path(std::path::Path::new(
            &workspace.workspace_path,
        )));
        // The dead helper is gone from the cache; start a fresh one and run the
        // command it was meant for. Failing here would make the caller retry, and a
        // caller that does not retry loses the command.
        let fresh = start_helper(workspace, runtime_pid, runtime_starttime_ticks)?;
        let fresh = Arc::new(Mutex::new(fresh));
        {
            let mut guard = HELPERS
                .get_or_init(|| Mutex::new(HashMap::new()))
                .lock()
                .map_err(|_| anyhow::anyhow!("persistent helper cache lock poisoned"))?;
            guard.insert(key, Arc::clone(&fresh));
        }
        return run_on_helper(
            fresh,
            workspace,
            runtime_pid,
            runtime_starttime_ticks,
            cwd,
            command,
        );
    }
    drop(alive);
    run_on_helper(
        helper,
        workspace,
        runtime_pid,
        runtime_starttime_ticks,
        cwd,
        command,
    )
}

/// Send one command through `helper` and account for what the outcome means.
fn run_on_helper(
    helper: Arc<Mutex<PersistentHelper>>,
    workspace: &WorkspaceMetadata,
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    cwd: &str,
    command: &[String],
) -> Result<PersistentCommandOutput> {
    let mut helper = helper
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
    let _ = publish_busy_marker(&busy_path, helper.child.id());
    let result = send_command(
        &mut helper,
        runtime_pid,
        runtime_starttime_ticks,
        workspace,
        cwd,
        command,
    );
    let _ = fs::remove_file(&busy_path);
    if let Err(error) = &result {
        if format!("{error:#}").contains(transport::HELPER_TIMEOUT_PREFIX) {
            // The helper stopped answering. It is holding this workspace's command
            // mutex, so leaving it in the cache would make every later command for
            // the workspace queue behind a socket that will never speak again.
            let _ = helper.child.kill();
            let _ = helper.child.wait();
            let _ = fs::remove_file(&helper.socket);
        }
    }
    result
}

pub(crate) fn invalidate(pid: u32, starttime_ticks: u64) {
    let Some(helpers) = HELPERS.get() else {
        return;
    };
    if let Ok(mut guard) = helpers.lock() {
        if let Some(helper) = guard.remove(&HelperKey {
            pid,
            starttime_ticks,
        }) {
            if let Ok(mut helper) = helper.lock() {
                let _ = helper.child.kill();
                let _ = helper.child.wait();
                let _ = fs::remove_file(&helper.socket);
            }
        }
    }
}

/// Record that this workspace is running a command, and which helper owns it.
///
/// Written atomically so a cancel never reads a half-written id.
fn publish_busy_marker(path: &std::path::Path, helper_pid: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("tmp");
    fs::write(&temp, format!("{helper_pid}\n"))?;
    fs::rename(&temp, path)?;
    Ok(())
}

pub(crate) fn output_to_string(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
#[path = "../../../../tests/src/workspace/session/persistent.rs"]
mod tests;
