//! A stop must act on the process the record names, not on whatever holds the pid.

use std::fs;
use std::process::Command;

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, destroy_workspace, stop_workspace, WorkspaceLimits, WorkspaceStatus,
};

use super::support::{prepare_cached_rootfs, process_starttime, root_only, state_dir};

/// A workspace record whose runtime identity is stale must never signal whatever
/// process holds that pid now.
///
/// A pid is not an identity: the kernel reuses pids, so a record that kept only
/// the number would let a stop kill an unrelated process that happened to inherit
/// it. The record carries the process start time as well, and this test points the
/// record at a live process whose start time does not match, then proves the stop
/// leaves that process untouched.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn stopping_a_workspace_never_signals_a_pid_it_does_not_own() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-pid-reuse");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-pid-reuse-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    // The workspace is deliberately never started. The record written below is the
    // shape an interrupted start leaves behind: it claims a running runtime, and
    // the pid it names has since been handed to an unrelated process.

    // An unrelated process, standing in for whatever holds the pid after reuse.
    let mut victim = Command::new("sleep")
        .arg("300")
        .spawn()
        .expect("spawn the unrelated process");
    let victim_pid = victim.id();
    let victim_starttime = process_starttime(victim_pid).expect("victim start time");

    // Point the record at the victim with a start time that is not its own. A pid
    // on its own cannot tell the process the record meant apart from whatever holds
    // the number now, which is why the start time is part of the identity.
    enclave::registry::with_registry_mut(&state, |registry| {
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox.id)
            .expect("sandbox record");
        let workspace = sandbox
            .workspaces
            .get_mut(&workspace.id)
            .expect("workspace record");
        workspace.status = WorkspaceStatus::Running;
        workspace.runtime_pid = Some(victim_pid);
        workspace.runtime_starttime_ticks = Some(victim_starttime.wrapping_add(1));
        Ok(())
    })
    .expect("record the stale runtime identity");

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");

    assert_eq!(
        process_starttime(victim_pid),
        Some(victim_starttime),
        "the stop signalled pid {victim_pid}, which it does not own"
    );

    let _ = victim.kill();
    let _ = victim.wait();

    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
