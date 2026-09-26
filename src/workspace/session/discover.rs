//! Finding a workspace's live session when its record does not name it.
//!
//! A start records the runtime's pid only when it commits, so between the session
//! starting and that commit the record describes no process. A stop that arrives in
//! that window, or a launch that fails in it, would otherwise release the
//! workspace's markers and leave the session running: the process keeps its
//! namespaces, its mounts, its private `/tmp`, and its cgroup membership, and once
//! the markers are gone nothing on the host names it.
//!
//! Two things name the session without the record. The pid file is written by the
//! session from inside its own namespace, so a pid file present after a launch
//! started belongs to that launch. The command line is the process's own: the
//! launcher and the helper it becomes both carry `--ready-file` under the
//! workspace's runtime directory, which no other workspace shares.

use super::*;

/// The live session of `workspace`, by pid file or by command line.
///
/// The pid file is tried first because it names the process directly. It can be
/// absent even while the session is alive — a session that has not reached the write
/// yet, or a stop that removed the runtime markers as it settled the record — so the
/// command line is the fallback rather than an afterthought.
///
/// Nothing here signals anything: the caller proves identity with the start time it
/// reads before signalling, and decides whether the process it found is one to end.
pub(crate) fn live_session_pid(workspace: &WorkspaceMetadata) -> Option<u32> {
    if let Some(pid) = read_pid_file(&runtime_pid_file(workspace))
        .ok()
        .filter(|pid| process_alive(*pid))
    {
        return Some(pid);
    }
    find_session_by_command_line(workspace)
}

/// The live session whose command line names this workspace's runtime directory.
///
/// The runtime directory is derived from the workspace id, so a match cannot belong
/// to another workspace. A zombie holds no resources and is skipped.
fn find_session_by_command_line(workspace: &WorkspaceMetadata) -> Option<u32> {
    let marker = runtime_dir(workspace).to_string_lossy().to_string();
    let entries = fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&cmdline);
        if cmdline.contains("workspace-session") && cmdline.contains(&marker) {
            return Some(pid);
        }
    }
    None
}
