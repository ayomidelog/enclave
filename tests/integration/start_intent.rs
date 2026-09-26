//! The two intent records a start writes before it touches the host.
//!
//! A start writes its journal record and the registry's `Starting` transition with
//! the address reserved for it, and both go down before the launch begins. They are
//! written together rather than one after the other, so the failure handling has to
//! cover the case where one of them lands and the other does not.

use std::fs;
use std::path::Path;

use enclave::sandbox::{create_sandbox, start_sandbox, stop_sandbox, BootstrapMethod};
use enclave::workspace::{create_workspace, destroy_workspace, start_workspace, WorkspaceLimits};

use super::support::{prepare_cached_rootfs, root_only, state_dir, SandboxCleanup};

fn workspace_record(state: &Path, workspace_id: &str) -> (String, Option<String>) {
    enclave::registry::with_registry(state, |registry| {
        let workspace = registry
            .sandboxes
            .values()
            .find_map(|sandbox| sandbox.workspaces.get(workspace_id))
            .expect("the test's workspace record is gone");
        Ok((
            workspace.status.as_str().to_string(),
            workspace.assigned_ip.clone(),
        ))
    })
    .expect("read the registry")
}

/// A start whose journal cannot be written must give back the reservation it took.
///
/// The journal is what describes the operation, so a start that cannot write it has
/// to stop. Stopping is not enough on its own: the reservation is already recorded by
/// then, and leaving it would keep the workspace `Starting` with an address held and
/// no operation that will ever finish it. The test makes the journal directory
/// unusable and asserts the workspace is back to a state a later start can use.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_start_whose_journal_cannot_be_written_releases_its_reservation() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-start-intent");
    prepare_cached_rootfs(&state, "bookworm");
    // The test asserts on a start that fails, so a failed assertion here is a real
    // possibility; the guard is what keeps a failing run from leaving a sandbox
    // and its mounts on the host.
    let mut cleanup = SandboxCleanup::new(state.clone());
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-intent-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    cleanup.record(&sandbox.id);
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    assert_eq!(workspace_record(&state, &workspace.id).0, "stopped");

    // Replace the journal directory with a file, so no record can be written.
    let journal_dir = state.join("operations");
    if journal_dir.is_dir() {
        fs::remove_dir_all(&journal_dir).expect("remove the journal directory");
    }
    fs::write(&journal_dir, b"").expect("block the journal directory");

    let error = start_workspace(&state, &sandbox.id, &workspace.id)
        .expect_err("a start with no journal must fail");
    assert!(
        format!("{error:#}").contains("operation journal"),
        "the error must name the journal: {error:#}"
    );

    let (status, assigned_ip) = workspace_record(&state, &workspace.id);
    assert_eq!(
        status, "stopped",
        "the failed start left the workspace {status} with the address {assigned_ip:?} held"
    );
    assert_eq!(
        assigned_ip, None,
        "the failed start did not give its reserved address back"
    );

    // And the workspace is usable again once the journal can be written.
    fs::remove_file(&journal_dir).expect("remove the blocker");
    let started = start_workspace(&state, &sandbox.id, &workspace.id)
        .expect("the start must succeed once the journal can be written");
    assert!(started.runtime_pid.is_some());

    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    drop(cleanup);
}
