//! Moving processes into a cgroup and controlling the ones already in it.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use super::layout::is_cgroup_v2_available;
use super::write_cgroup_value;

pub fn add_process_to_cgroup(cgroup_path: &Path, pid: u32) -> Result<()> {
    let procs_file = cgroup_path.join("cgroup.procs");
    write_cgroup_value(&procs_file, &pid.to_string())
        .with_context(|| format!("failed to add pid {} to cgroup", pid))
}

/// Terminate every process currently attached to a cgroup in one kernel
/// operation. This is substantially faster and more reliable than walking a
/// large process tree and signalling descendants individually.
pub fn kill_cgroup_members(cgroup_path: &Path) -> Result<bool> {
    if !is_cgroup_v2_available() {
        return Ok(false);
    }

    let kill_path = cgroup_path.join("cgroup.kill");
    if !kill_path.exists() {
        return Ok(false);
    }

    write_cgroup_value(&kill_path, "1")
        .with_context(|| format!("failed to kill processes in {}", cgroup_path.display()))?;
    Ok(true)
}

/// Freeze or thaw all processes in a cgroup without destroying its runtime.
pub fn set_cgroup_frozen(cgroup_path: &Path, frozen: bool) -> Result<bool> {
    if !is_cgroup_v2_available() {
        return Ok(false);
    }
    let freeze_path = cgroup_path.join("cgroup.freeze");
    if !freeze_path.exists() {
        return Ok(false);
    }
    write_cgroup_value(&freeze_path, if frozen { "1" } else { "0" })
        .with_context(|| format!("failed to set cgroup.freeze for {}", cgroup_path.display()))?;
    Ok(true)
}

/// Confirm that a runtime PID is still a member of the managed cgroup before
/// applying a destructive operation to that cgroup.
pub fn cgroup_contains_pid(cgroup_path: &Path, pid: u32) -> Result<bool> {
    let contents = fs::read_to_string(cgroup_path.join("cgroup.procs")).with_context(|| {
        format!(
            "failed to read {}",
            cgroup_path.join("cgroup.procs").display()
        )
    })?;
    Ok(contents.lines().any(|line| line.trim() == pid.to_string()))
}
