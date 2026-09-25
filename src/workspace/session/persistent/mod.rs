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
    let mut helper = helper
        .lock()
        .map_err(|_| anyhow::anyhow!("persistent helper lock poisoned"))?;
    if helper.child.try_wait()?.is_some() {
        drop(helper);
        if let Ok(mut guard) = helpers.lock() {
            guard.remove(&key);
        }
        bail!("persistent workspace session helper exited unexpectedly");
    }
    send_command(
        &mut helper,
        runtime_pid,
        runtime_starttime_ticks,
        workspace,
        cwd,
        command,
    )
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

pub(crate) fn output_to_string(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::output_to_string;

    #[test]
    fn persistent_output_decodes_lossy_utf8() {
        assert_eq!(output_to_string(vec![b'o', b'k']), "ok");
    }
}
