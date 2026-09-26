//! What a pause and a resume owe the caller when their metadata commit fails.
//!
//! Both operations do the host work first and record it second: a pause freezes the
//! sandbox cgroup and then writes the new status, and a resume thaws it and then
//! writes the new status. The write can fail, and a failure that left the cgroup
//! frozen while the record still said the sandbox was running would be the worst of
//! both: every process in it stopped, and nothing describing why. Each operation
//! therefore rolls the cgroup back to the state the record still describes, and these
//! are the two tests that pin that.
//!
//! The failure is injected where it is real rather than through a hook. The sandbox
//! status is persisted to sandbox.json in the sandbox directory, and a directory
//! cannot be replaced by a rename, so occupying that path makes the write fail while
//! leaving every other file, including the registry the read path uses, intact.

use std::fs;
use std::path::{Path, PathBuf};

use enclave::operation::{load, OperationStatus};
use enclave::sandbox::{
    create_sandbox, destroy_sandbox, pause_sandbox, resume_sandbox, sandbox_status, start_sandbox,
    stop_sandbox, BootstrapMethod, SandboxStatus,
};
use enclave::workspace::{
    create_workspace, destroy_workspace, start_workspace, stop_workspace, WorkspaceLimits,
};

use super::support::{prepare_cached_rootfs, root_only, state_dir, SandboxCleanup};

fn sandbox_cgroup_path(sandbox_id: &str) -> PathBuf {
    Path::new("/sys/fs/cgroup").join(format!("enclave-sb-{sandbox_id}"))
}

/// Whether the sandbox cgroup is frozen, as the kernel reports it.
///
/// A missing cgroup reads as absent rather than as "not frozen", because a test that
/// accepted a missing cgroup would pass on a pause that never got as far as freezing
/// anything.
fn cgroup_is_frozen(cgroup: &Path) -> Option<bool> {
    let raw = fs::read_to_string(cgroup.join("cgroup.freeze")).ok()?;
    Some(raw.trim() == "1")
}

/// Occupy the path the sandbox status is persisted to with a directory.
///
/// The atomic write used here writes a temporary file beside the target and renames
/// it over the target, and a rename onto a directory fails. The registry the read path
/// uses is a different file, so every read still answers.
fn block_sandbox_metadata_write(sandbox_path: &str) -> PathBuf {
    let metadata_path = Path::new(sandbox_path).join("sandbox.json");
    // The path is a file to begin with, so it is removed before the directory that
    // replaces it can be created.
    if metadata_path.is_file() {
        fs::remove_file(&metadata_path).expect("remove the sandbox metadata file");
    }
    fs::create_dir_all(metadata_path.join("blocked")).expect("occupy the sandbox metadata path");
    assert!(
        metadata_path.is_dir(),
        "the sandbox metadata path must be a directory for the write to fail"
    );
    metadata_path
}

/// The most recent journal record for one kind of operation.
fn latest_journal(state: &Path, kind: &str) -> enclave::operation::OperationRecord {
    let mut newest: Option<enclave::operation::OperationRecord> = None;
    for entry in fs::read_dir(state.join("operations"))
        .expect("the operations must have written a journal")
        .flatten()
    {
        let path = entry.path();
        let Some(id) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        let record = load(state, id).expect("read the journal record");
        if record.kind != kind {
            continue;
        }
        if newest
            .as_ref()
            .is_none_or(|current| record.updated_at >= current.updated_at)
        {
            newest = Some(record);
        }
    }
    newest.unwrap_or_else(|| panic!("no journal record for {kind}"))
}

/// A sandbox with one running workspace, which is what a pause needs to have
/// anything to freeze.
/// The sandbox cleanup the caller has to keep alive for the length of the test.
///
/// A test that fails part way through leaves a sandbox cgroup and a state directory
/// behind unless something removes them, and the failure this test injects is exactly
/// the case where the sandbox cannot be torn down by the normal path.
fn running_sandbox(
    state: &Path,
    name: &str,
    cleanup: &mut SandboxCleanup,
) -> (enclave::sandbox::SandboxMetadata, String) {
    prepare_cached_rootfs(state, "bookworm");
    let sandbox = create_sandbox(
        state,
        "debootstrap",
        name,
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(state, &sandbox.id).expect("start sandbox");
    cleanup.record(&sandbox.id);
    let workspace = create_workspace(state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    start_workspace(state, &sandbox.id, &workspace.id).expect("start workspace");
    (sandbox, workspace.id)
}

fn stop_workspace_and_destroy(state: &Path, sandbox_id: &str, workspace_id: &str) {
    stop_workspace(state, sandbox_id, workspace_id).expect("stop workspace");
    destroy_workspace(state, sandbox_id, workspace_id).expect("destroy workspace");
    stop_sandbox(state, sandbox_id).expect("stop sandbox");
    destroy_sandbox(state, sandbox_id).expect("destroy sandbox");
}

/// A pause whose status write fails must thaw the cgroup it just froze.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_pause_whose_commit_fails_thaws_the_cgroup_it_froze() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-pause-commit");
    let mut cleanup = SandboxCleanup::new(state.clone());
    let (sandbox, workspace_id) =
        running_sandbox(&state, "itest-pause-commit-sandbox", &mut cleanup);
    let cgroup = sandbox_cgroup_path(&sandbox.id);
    assert_eq!(
        cgroup_is_frozen(&cgroup),
        Some(false),
        "a running sandbox must not be frozen before the pause"
    );

    let metadata_path = block_sandbox_metadata_write(&sandbox.sandbox_path);
    let error = pause_sandbox(&state, &sandbox.id).expect_err("the commit must fail");
    assert!(
        format!("{error:#}").contains("sandbox metadata"),
        "the failure must name what could not be written: {error:#}"
    );

    // The rollback is the whole point: the cgroup is thawed again, and the record the
    // operator reads still describes a running sandbox.
    assert_eq!(
        cgroup_is_frozen(&cgroup),
        Some(false),
        "a pause that could not commit left the cgroup frozen"
    );
    let status = sandbox_status(&state, &sandbox.id).expect("read the sandbox status");
    assert_eq!(
        status.status,
        SandboxStatus::Running,
        "a pause that could not commit left the sandbox described as {:?}",
        status.status
    );
    let record = latest_journal(&state, "sandbox.pause");
    assert_eq!(
        record.status,
        OperationStatus::Failed,
        "the pause journal record is {:?} after the commit failed",
        record.status
    );

    // The probe is not vacuous: the same pause succeeds once the path is writable, so
    // an unfrozen cgroup above means the rollback ran rather than the freeze never
    // happening.
    fs::remove_dir_all(&metadata_path).expect("clear the blocking directory");
    pause_sandbox(&state, &sandbox.id).expect("the pause must succeed once the path is writable");
    assert_eq!(
        cgroup_is_frozen(&cgroup),
        Some(true),
        "a pause that committed must leave the cgroup frozen"
    );

    resume_sandbox(&state, &sandbox.id).expect("resume sandbox");
    stop_workspace_and_destroy(&state, &sandbox.id, &workspace_id);
}

/// A resume whose status write fails must refreeze the cgroup it just thawed.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_resume_whose_commit_fails_refreezes_the_cgroup_it_thawed() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-resume-commit");
    let mut cleanup = SandboxCleanup::new(state.clone());
    let (sandbox, workspace_id) =
        running_sandbox(&state, "itest-resume-commit-sandbox", &mut cleanup);
    let cgroup = sandbox_cgroup_path(&sandbox.id);
    pause_sandbox(&state, &sandbox.id).expect("pause sandbox");
    assert_eq!(cgroup_is_frozen(&cgroup), Some(true));

    let metadata_path = block_sandbox_metadata_write(&sandbox.sandbox_path);
    let error = resume_sandbox(&state, &sandbox.id).expect_err("the commit must fail");
    assert!(
        format!("{error:#}").contains("sandbox metadata"),
        "the failure must name what could not be written: {error:#}"
    );

    assert_eq!(
        cgroup_is_frozen(&cgroup),
        Some(true),
        "a resume that could not commit left the cgroup thawed"
    );
    let status = sandbox_status(&state, &sandbox.id).expect("read the sandbox status");
    assert_eq!(
        status.status,
        SandboxStatus::Paused,
        "a resume that could not commit left the sandbox described as {:?}",
        status.status
    );
    let record = latest_journal(&state, "sandbox.resume");
    assert_eq!(
        record.status,
        OperationStatus::Failed,
        "the resume journal record is {:?} after the commit failed",
        record.status
    );

    fs::remove_dir_all(&metadata_path).expect("clear the blocking directory");
    resume_sandbox(&state, &sandbox.id).expect("the resume must succeed once the path is writable");
    assert_eq!(
        cgroup_is_frozen(&cgroup),
        Some(false),
        "a resume that committed must leave the cgroup thawed"
    );

    stop_workspace_and_destroy(&state, &sandbox.id, &workspace_id);
}
