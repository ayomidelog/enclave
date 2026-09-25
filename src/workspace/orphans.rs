//! Finding the runtime of a workspace whose own records are gone.
//!
//! `workspace.json` is the only file that records a runtime's pid, so losing it
//! used to mean losing the workspace: repair saw a directory with no metadata,
//! treated it as a leftover, and deleted it — while the runtime kept running with
//! its cgroup, its interface, and its firewall rules, now owned by nothing.
//!
//! The workspace directory carries two markers that survive the loss of its
//! metadata. `ns/pid.ref` and `ns/mnt.ref` record the namespace inodes of the
//! running session, and the session's cgroup is named from the sandbox and
//! workspace ids rather than from anything in the metadata. Either one is enough
//! to prove that a live process still owns the directory.
//!
//! Discovery only ever reads. Nothing here signals a process: a pid found this way
//! is reported for an operator to act on, and the namespace inode is what makes it
//! the right process rather than a reused pid.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::session;

/// A live owner found for a workspace directory that has no metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanRuntime {
    /// The process whose namespaces match the directory's recorded references.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_pid: Option<u32>,
    /// The namespace inode from `ns/pid.ref`, which is what identified it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid_namespace: Option<String>,
    /// An Enclave cgroup for this workspace that still holds processes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup: Option<PathBuf>,
}

impl OrphanRuntime {
    /// One line naming what is still alive and how it was found.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(pid) = self.runtime_pid {
            match self.pid_namespace.as_deref() {
                Some(namespace) => parts.push(format!("runtime pid {pid} in {namespace}")),
                None => parts.push(format!("runtime pid {pid}")),
            }
        }
        if let Some(cgroup) = self.cgroup.as_deref() {
            parts.push(format!("cgroup {}", cgroup.display()));
        }
        parts.join("; ")
    }
}

/// Whether a live process still owns `workspace_dir`.
///
/// The workspace id is needed as well as the directory because the cgroup is
/// named from the sandbox and workspace ids, and the sandbox id because the cgroup
/// is nested under the sandbox's own group.
pub fn find_orphan_runtime(
    workspace_dir: &Path,
    sandbox_id: &str,
    workspace_id: &str,
) -> Option<OrphanRuntime> {
    let pid_namespace = read_namespace_marker(&workspace_dir.join("ns").join("pid.ref"));
    let runtime_pid = read_runtime_pid_file(&workspace_dir.join("runtime").join("session.pid"))
        // The pid file names the session directly, so it is the precise answer
        // whenever it survived. It is only trusted when its namespace agrees with
        // the marker, so a reused pid cannot be mistaken for the runtime.
        .filter(|pid| {
            pid_namespace.is_none()
                || process_namespace(*pid).as_deref() == pid_namespace.as_deref()
        })
        .or_else(|| pid_namespace.as_deref().and_then(find_process_in_namespace));
    let cgroup = live_workspace_cgroup(sandbox_id, workspace_id, runtime_pid);

    if runtime_pid.is_none() && cgroup.is_none() {
        return None;
    }
    Some(OrphanRuntime {
        runtime_pid,
        pid_namespace,
        cgroup,
    })
}

/// The namespace inode a marker file records, e.g. `pid:[4026532812]`.
fn read_namespace_marker(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    let value = raw.trim();
    // A stopped workspace leaves `unassigned`, which is not an owner.
    if value.is_empty() || value == "unassigned" {
        return None;
    }
    Some(value.to_string())
}

/// The live process whose pid namespace is `namespace`.
///
/// A namespace inode is unique for the life of the namespace and is not reused
/// while any process holds it, so a match is proof of ownership rather than a
/// guess about a pid. The scan is over `/proc` because the pid itself was never
/// recorded anywhere that survived.
fn find_process_in_namespace(namespace: &str) -> Option<u32> {
    let entries = fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        if process_namespace(pid).as_deref() == Some(namespace) {
            return Some(pid);
        }
    }
    None
}

/// The pid namespace a process is in, e.g. `pid:[4026532812]`.
fn process_namespace(pid: u32) -> Option<String> {
    fs::read_link(format!("/proc/{pid}/ns/pid"))
        .ok()
        .map(|link| link.to_string_lossy().to_string())
}

/// The pid a session pid file records, when that process is still alive.
fn read_runtime_pid_file(path: &Path) -> Option<u32> {
    let pid = session::read_pid_file(path).ok()?;
    session::process_matches(pid, None).then_some(pid)
}

/// An Enclave cgroup for this workspace that still holds processes.
fn live_workspace_cgroup(
    sandbox_id: &str,
    workspace_id: &str,
    runtime_pid: Option<u32>,
) -> Option<PathBuf> {
    let managed = super::runtime_limits::workspace_cgroup_path(sandbox_id, workspace_id);
    if cgroup_has_processes(&managed) {
        return Some(managed);
    }
    // A workspace started before cgroup names were derived from the resource ids
    // has its cgroup at the cgroup root named from the pid. That shape is still
    // cleaned up, so it is still evidence of a live owner.
    if let Some(pid) = runtime_pid {
        let legacy = PathBuf::from("/sys/fs/cgroup")
            .join(super::runtime_limits::legacy_workspace_cgroup_name(pid));
        if cgroup_has_processes(&legacy) {
            return Some(legacy);
        }
    }
    None
}

/// Whether `path` is a cgroup with at least one process attached.
fn cgroup_has_processes(path: &Path) -> bool {
    fs::read_to_string(path.join("cgroup.procs"))
        .map(|procs| !procs.trim().is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "../../tests/src/workspace/orphans.rs"]
mod tests;
