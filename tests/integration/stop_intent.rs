//! What a stop records before it touches a runtime, and what it leaves behind when
//! it cannot.
//!
//! A stop writes its journal record and the registry's `Stopping` transition before
//! it signals the runtime, and the two are written together rather than one after the
//! other. They can therefore land independently, and a stop that cannot write its
//! journal must not leave the workspace transitional with nothing to finish it.

use std::fs;
use std::path::Path;

use enclave::sandbox::{create_sandbox, start_sandbox, stop_sandbox, BootstrapMethod};
use enclave::workspace::{
    create_workspace, destroy_workspace, start_workspace, stop_workspace, WorkspaceLimits,
};

use super::support::{prepare_cached_rootfs, root_only, state_dir, SandboxCleanup};

fn workspace_record(state: &Path, workspace_id: &str) -> (String, Option<u32>) {
    enclave::registry::with_registry(state, |registry| {
        let workspace = registry
            .sandboxes
            .values()
            .find_map(|sandbox| sandbox.workspaces.get(workspace_id))
            .expect("the test's workspace record is gone");
        Ok((workspace.status.as_str().to_string(), workspace.runtime_pid))
    })
    .expect("read the registry")
}

/// A stop whose journal cannot be written must give back the `Stopping` transition.
///
/// The journal is what describes the operation, so a stop that cannot write it has to
/// stop. The `Stopping` transition may already be on disk by then, and a workspace
/// left `Stopping` reads as an operation still in flight: it refuses the next start
/// until a repair rolls it back, while the runtime it describes is still running. The
/// test blocks the journal directory, asserts the stop fails naming the journal, and
/// asserts the workspace is back to `running` with its runtime untouched.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_stop_whose_journal_cannot_be_written_puts_the_status_back() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-stop-intent");
    prepare_cached_rootfs(&state, "bookworm");
    // The test asserts on a stop that fails, so a failed assertion here is a real
    // possibility; the guard is what keeps a failing run from leaving a sandbox and
    // its mounts on the host.
    let mut cleanup = SandboxCleanup::new(state.clone());
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-stop-intent-sandbox",
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

    // Replace the journal directory with a file, so no record can be written.
    let journal_dir = state.join("operations");
    if journal_dir.is_dir() {
        fs::remove_dir_all(&journal_dir).expect("remove the journal directory");
    }
    fs::write(&journal_dir, b"").expect("block the journal directory");

    let error = stop_workspace(&state, &sandbox.id, &workspace.id)
        .expect_err("a stop with no journal must fail");
    assert!(
        format!("{error:#}").contains("operation journal"),
        "the error must name the journal: {error:#}"
    );

    let (status, recorded_pid) = workspace_record(&state, &workspace.id);
    assert_eq!(
        status, "running",
        "the failed stop left the workspace {status} with nothing to finish it"
    );
    assert_eq!(
        recorded_pid,
        Some(runtime_pid),
        "the failed stop dropped the runtime identity"
    );
    assert!(
        Path::new(&format!("/proc/{runtime_pid}")).exists(),
        "the failed stop signalled a runtime it never had a journal for"
    );

    // And the stop goes through once the journal can be written.
    fs::remove_file(&journal_dir).expect("remove the blocker");
    let stopped = stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop the workspace");
    assert_eq!(stopped.status.as_str(), "stopped");
    assert_eq!(stopped.runtime_pid, None);

    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    drop(cleanup);
}
