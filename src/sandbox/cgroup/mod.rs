//! The cgroup v2 resources a sandbox and its workspaces are placed in.
//!
//! A sandbox gets one cgroup, and each workspace with declared limits gets a
//! child of it, so a workspace's CPU, memory, and process limits are enforced by
//! the kernel and a sandbox can be killed or frozen in one operation. The files
//! are split by what they touch: layout resolves and manages the directory tree,
//! config turns declared limits into cgroup values, process moves and signals
//! members, and stats reads usage back.

mod config;
mod layout;
mod process;
mod stats;

use anyhow::{Context, Result};

pub use config::CgroupConfig;
pub use layout::{
    available_controllers, create_workspace_cgroup, ensure_sandbox_cgroup, ensure_workspace_cgroup,
    is_cgroup_v2_available, remove_cgroup_path, remove_workspace_cgroup, runtime_cgroup_path,
    sandbox_cgroup_name,
};
pub use process::{
    add_process_to_cgroup, cgroup_contains_pid, kill_cgroup_members, set_cgroup_frozen,
};
pub use stats::{read_cgroup_stats, CgroupStats};

#[cfg(test)]
pub(crate) use layout::validate_cgroup_name;

/// Write one value file. Every cgroup control is a write to a file in the
/// cgroup's directory, so the write and its error context live in one place.
pub(super) fn write_cgroup_value(path: &std::path::Path, value: &str) -> Result<()> {
    std::fs::write(path, value)
        .with_context(|| format!("failed to write '{}' to {}", value, path.display()))
}

#[cfg(test)]
#[path = "../../../tests/src/sandbox/cgroup.rs"]
mod tests;
