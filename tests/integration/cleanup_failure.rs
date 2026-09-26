//! What a cleanup that cannot finish leaves behind.
//!
//! A teardown that releases everything reports success; a teardown that cannot
//! release something has to say so and leave the evidence to finish the job. The
//! dangerous third outcome is a teardown that reports success while something is
//! still held, because the record that described it is then rewritten as stopped
//! and nothing on the host names what is left.
//!
//! The obstruction here is a nested cgroup. The kernel refuses to remove a cgroup
//! that still has a child, so the workspace's own removal cannot finish, and that
//! is a state the plan's `cleanup_failed` criterion is about: the record has to
//! stay transitional rather than claim a stop that did not happen.

use std::fs;
use std::path::PathBuf;

use enclave::operation::{load, OperationStatus};
use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, destroy_workspace, start_workspace, stop_workspace, WorkspaceLimits,
};

use super::support::{
    prepare_cached_rootfs, root_only, state_dir, workspace_cgroup_path, SandboxCleanup,
};

/// The workspace record's status, read from the registry and from its own file.
///
/// Both copies matter: the registry is what the daemon answers from, and the
/// on-disk file is what repair adopts when the two disagree.
fn recorded_status(state: &std::path::Path, workspace_path: &str) -> (String, String) {
    let from_registry = enclave::registry::with_registry(state, |registry| {
        Ok(registry
            .sandboxes
            .values()
            .find_map(|sandbox| sandbox.workspaces.values().next())
            .map(|workspace| workspace.status.as_str().to_string()))
    })
    .expect("read the registry")
    .expect("the test's workspace record is gone");
    let raw = fs::read_to_string(std::path::Path::new(workspace_path).join("workspace.json"))
        .expect("read the workspace metadata");
    let on_disk: serde_json::Value = serde_json::from_str(&raw).expect("parse the metadata");
    (
        from_registry,
        on_disk["status"]
            .as_str()
            .expect("the metadata carries a status")
            .to_string(),
    )
}

/// The newest `workspace.stop` journal record, as `(status, detail)`.
fn last_stop_journal(state: &std::path::Path) -> (OperationStatus, String) {
    let root = state.join("operations");
    let mut newest: Option<(String, OperationStatus, String)> = None;
    for entry in fs::read_dir(&root).expect("read the journal").flatten() {
        let Some(id) = entry
            .path()
            .file_stem()
            .and_then(|name| name.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let Ok(record) = load(state, &id) else {
            continue;
        };
        if record.kind != "workspace.stop" {
            continue;
        }
        if newest
            .as_ref()
            .is_none_or(|(at, _, _)| record.updated_at >= *at)
        {
            newest = Some((
                record.updated_at.clone(),
                record.status,
                record.error.clone().unwrap_or_default(),
            ));
        }
    }
    let (_, status, detail) = newest.expect("the stop wrote a journal record");
    (status, detail)
}

/// A stop that cannot release a resource must leave the record transitional and
/// name the resource it could not release.
///
/// The obstruction is a cgroup nested inside the workspace's own, which is a
/// shape an operator or a tool can create and which Enclave's removal does not
/// know about. The stop has to fail, naming the workspace cgroup it could not
/// remove, and the record has to stay `stopping`: a record rewritten as `stopped`
/// would take away the only description of the cgroup that is still held, and the
/// next start would stack a runtime on top of it. Repair finishes the stop once the
/// obstruction is gone, which is what makes the transitional state a recovery point
/// rather than a dead end.
#[test]
#[ignore = "requires root privileges, cgroup v2, and namespace/mount support"]
fn a_stop_that_cannot_release_a_cgroup_leaves_recovery_evidence() {
    if !root_only() {
        return;
    }
    if !enclave::sandbox::cgroup::is_cgroup_v2_available() {
        return;
    }

    let state = state_dir("enclave-int-cleanup-failure");
    prepare_cached_rootfs(&state, "bookworm");
    let mut cleanup = SandboxCleanup::new(state.clone());
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-cleanup-failure-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    cleanup.record(&sandbox.id);
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");

    let cgroup = workspace_cgroup_path(&sandbox.id, &workspace.id);
    assert!(
        cgroup.is_dir(),
        "the running workspace owns {}",
        cgroup.display()
    );
    let nested: PathBuf = cgroup.join("probe-nested");
    fs::create_dir(&nested).expect("create the nested cgroup");

    let error = stop_workspace(&state, &sandbox.id, &workspace.id)
        .expect_err("a stop that cannot remove a cgroup must fail");
    let message = format!("{error:#}");
    assert!(
        message.contains(&cgroup.display().to_string()),
        "the failure must name the cgroup it could not remove: {message}"
    );
    assert!(
        message.contains("still busy"),
        "the failure must say why the cgroup survived: {message}"
    );

    // The record is the only description of what is still held, so it has to
    // survive as a transitional state rather than being rewritten as stopped.
    let (registry_status, disk_status) = recorded_status(&state, &workspace.workspace_path);
    assert_eq!(
        registry_status, "stopping",
        "the registry claims a stop that did not happen"
    );
    assert_eq!(
        disk_status, "stopping",
        "the on-disk copy claims a stop that did not happen"
    );

    // And the journal says why, so an operator does not have to reproduce it.
    let (status, detail) = last_stop_journal(&state);
    assert_eq!(
        status,
        OperationStatus::Failed,
        "the stop journal is {status:?}"
    );
    assert!(
        detail.contains(&cgroup.display().to_string()),
        "the journal must name the resource it could not release: {detail}"
    );

    // Repair is the recovery point: with the obstruction gone, the stop finishes
    // and the record settles.
    fs::remove_dir(&nested).expect("remove the nested cgroup");
    enclave::sandbox::reconcile_runtime_state(&state).expect("reconcile the transitional record");

    let (registry_status, disk_status) = recorded_status(&state, &workspace.workspace_path);
    assert_eq!(registry_status, "stopped", "repair did not finish the stop");
    assert_eq!(
        disk_status, "stopped",
        "repair did not persist the settled state"
    );
    assert!(!cgroup.exists(), "repair left {} behind", cgroup.display());

    // The workspace is usable again, which is what the recovery point is for.
    let restarted = start_workspace(&state, &sandbox.id, &workspace.id)
        .expect("the workspace must start once its cgroup is released");
    assert!(restarted.runtime_pid.is_some());

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(&state);
}
