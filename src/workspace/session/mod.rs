mod idmap;
mod namespace_cache;
mod persistent;
mod process;
mod script;
mod security;
mod userns;

use std::collections::BTreeSet;
use std::fs;
use std::fs::OpenOptions;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::types::WorkspaceMetadata;

pub(crate) use idmap::workspace_bind_mount_idmap_option;
pub(crate) use namespace_cache::{duplicate_for_child, make_inheritable, raw_fds};
pub(crate) use persistent::{
    execute_persistent_command, output_to_string, MAX_HELPER_OUTPUT_BYTES,
};
pub use process::{
    count_processes_in_pid_namespace, process_alive, process_matches, process_resource_usage,
    process_starttime_ticks, read_namespace_refs,
};
pub(crate) use script::WORKSPACE_SESSION_SCRIPT;
pub(crate) use security::{
    apply_exec_restrictions, apply_session_restrictions, detach_old_root, mask_runtime_paths,
    tighten_namespace_mounts,
};
pub(crate) use userns::{detect_user_namespace_mode, UserNamespaceMode};

const START_TIMEOUT: Duration = Duration::from_secs(5);
// Dedicated cgroups provide a fast fallback for the remaining process tree.
// Keep graceful shutdown short so every workspace does not pay a fixed
// multi-second delay before cgroup.kill is used.
const STOP_TIMEOUT: Duration = Duration::from_millis(500);
const POST_KILL_TIMEOUT: Duration = Duration::from_millis(500);
const SESSION_HELPER_BASENAME: &str = "session-helper";
const SELF_EXE_PATH: &str = "/proc/self/exe";
const HELPER_OVERRIDE_ENV: &str = "ENCLAVE_SELF_EXE";
const TEST_BINARY_ENV: &str = "CARGO_BIN_EXE_enclave";
static SESSION_HELPER_PREPARE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub pid: u32,
    pub starttime_ticks: u64,
    pub mount_ns: String,
    pub pid_ns: String,
}

#[derive(Debug, Default)]
pub struct BatchStopResult {
    pub stopped_pids: BTreeSet<u32>,
    pub failed_pids: BTreeSet<u32>,
}

mod helper;
mod paths;
mod start;
mod stop;

// Runtime control.
pub use start::start_session;
#[cfg(test)]
pub use stop::stop_sessions_batch;
pub use stop::{stop_session, stop_sessions_batch_with_hook};

// Runtime layout and namespace reference files.
pub use paths::{
    clear_namespace_ref_files, namespace_ref_files_exist, namespace_ref_paths,
    namespace_refs_match_runtime, runtime_log_file, runtime_pid_file, runtime_ready_file,
    write_namespace_ref_values,
};

// Shared between the session modules above.
#[cfg(test)]
pub(crate) use helper::{infer_workspace_helper_from_current_exe, session_helper_path};
pub(crate) use helper::{prepare_session_helper, resolve_session_helper_source};
pub(crate) use paths::{ensure_runtime_layout, sandbox_runtime_dir};
#[cfg(test)]
pub(crate) use start::{launch_userns_args, setgroups_args};

#[cfg(test)]
#[path = "../../../tests/src/workspace/session/mod.rs"]
mod tests;
