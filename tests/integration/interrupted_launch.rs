//! Ending the session of a launch that was interrupted before it committed.
//!
//! A start records the runtime's pid only when it commits, so between the session
//! becoming ready and that commit the record describes a workspace with no runtime
//! while a real process is running. A stop that arrives in that window, or a start
//! that fails in it, has to end that process anyway: the runtime markers are removed
//! as the record settles, and a session left behind is then a live process holding
//! namespaces, mounts, a private `/tmp`, and cgroup membership that nothing on the
//! host names any more.
//!
//! The window is not hypothetical. It is what a daemon that dies between the
//! readiness signal and the registry commit leaves, and it is what a stop racing a
//! start lands in. The test builds the state directly rather than racing for it,
//! because the failure is the state rather than the timing.

use std::fs;
use std::path::Path;

use enclave::sandbox::{create_sandbox, destroy_sandbox, start_sandbox, BootstrapMethod};
use enclave::workspace::{
    create_workspace, destroy_workspace, start_workspace, stop_workspace, WorkspaceLimits,
};

use super::support::{
    prepare_cached_rootfs, root_only, session_processes_for, state_dir, SandboxCleanup,
};

/// Rewrite a workspace's record as an interrupted launch left it.
///
/// The registry and the workspace's own file both describe the workspace, so both
/// are put back to `starting` with no runtime identity. The session pid file is
/// removed as well, which is the state a stop overlapping the launch produces when
/// it removes the runtime markers while settling the record.
fn record_interrupted_launch(state: &Path, workspace_path: &str, workspace_id: &str) {
    enclave::registry::with_registry_mut(state, |registry| {
        let workspace = registry
            .sandboxes
            .values_mut()
            .find_map(|sandbox| sandbox.workspaces.get_mut(workspace_id))
            .expect("the test's workspace record is gone");
        workspace.status = enclave::workspace::WorkspaceStatus::Starting;
        workspace.runtime_pid = None;
        workspace.runtime_starttime_ticks = None;
        Ok(())
    })
    .expect("rewrite the registry record");

    let metadata_path = Path::new(workspace_path).join("workspace.json");
    let mut record: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&metadata_path).expect("read the workspace metadata"),
    )
    .expect("parse the workspace metadata");
    record["status"] = serde_json::json!("starting");
    record["runtime_pid"] = serde_json::Value::Null;
    record["runtime_starttime_ticks"] = serde_json::Value::Null;
    fs::write(
        &metadata_path,
        serde_json::to_string_pretty(&record).expect("render the workspace metadata"),
    )
    .expect("write the workspace metadata");

    fs::remove_file(Path::new(workspace_path).join("runtime/session.pid"))
        .expect("remove the session pid file");
}

/// A stop of a workspace whose launch never committed must still end its session.
///
/// The record names no pid, so the session is found the way the launch itself would
/// have been: by the pid file it wrote, or by its own command line when that file is
/// gone. Without that, the stop reports a clean teardown while the session keeps
/// running, and the record is rewritten as stopped so no later repair can find it.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_stop_ends_the_session_of_a_launch_that_never_committed() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-interrupted-launch");
    prepare_cached_rootfs(&state, "bookworm");
    let mut cleanup = SandboxCleanup::new(state.clone());
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-interrupted-launch-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    cleanup.record(&sandbox.id);
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let runtime_pid = started
        .runtime_pid
        .expect("the started workspace has a runtime");
    let sandbox_path = Path::new(&sandbox.sandbox_path);
    assert_eq!(
        session_processes_for(sandbox_path),
        vec![runtime_pid],
        "the running workspace has exactly its own session"
    );

    record_interrupted_launch(&state, &workspace.workspace_path, &workspace.id);

    let stopped = stop_workspace(&state, &sandbox.id, &workspace.id)
        .expect("a stop must release the session the interrupted launch left");
    assert_eq!(stopped.status.as_str(), "stopped");
    assert_eq!(stopped.runtime_pid, None);
    assert!(
        session_processes_for(sandbox_path).is_empty(),
        "the stop reported a clean teardown while the session is still running"
    );

    // The workspace is usable again, which is what makes the interrupted launch a
    // recovery point rather than a lost runtime.
    let restarted = start_workspace(&state, &sandbox.id, &workspace.id)
        .expect("the workspace must start again after the interrupted launch");
    assert!(restarted.runtime_pid.is_some());

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(&state);
}
