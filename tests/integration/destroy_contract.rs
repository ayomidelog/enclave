//! What a destroy has to prove, beyond the files being gone.
//!
//! A destroy removes a workspace's record, so afterwards there is nothing left that
//! describes what the workspace owned. Everything the destroy had to release
//! therefore has to be verified before the record goes, and the pieces that are easy
//! to miss are the ones that outlive a directory: the sandbox rootfs bind mount, the
//! sandbox cgroup, and the operation journal that says what happened.

use std::fs;
use std::path::Path;

use enclave::operation::{load, OperationStatus};
use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{create_workspace, destroy_workspace, start_workspace, WorkspaceLimits};

use super::support::{prepare_cached_rootfs, root_only, state_dir};

fn is_mountpoint(path: &str) -> bool {
    fs::read_to_string("/proc/self/mountinfo")
        .map(|raw| {
            raw.lines()
                .any(|line| line.split_whitespace().nth(4) == Some(path))
        })
        .unwrap_or(false)
}

fn sandbox_cgroup_path(sandbox_id: &str) -> std::path::PathBuf {
    Path::new("/sys/fs/cgroup").join(format!("enclave-sb-{sandbox_id}"))
}

/// The journal records a destroy left behind must be terminal.
///
/// A record that is still `planned` or `running` after the operation finished is
/// what doctor reports as an unfinished operation, and what a later daemon start
/// would try to reconcile. A destroy that leaves one is a destroy whose outcome
/// recovery cannot know, so the test reads every record the two destroys wrote and
/// asserts each reached a terminal state.
fn assert_journal_terminal(state: &Path) {
    let root = state.join("operations");
    let entries = fs::read_dir(&root).expect("the destroys must have written a journal");
    let mut checked = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(id) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        let record = load(state, id).expect("read the journal record");
        assert!(
            matches!(
                record.status,
                OperationStatus::Succeeded | OperationStatus::Failed
            ),
            "journal record {} ({}) is {:?} after the operation finished",
            record.id,
            record.kind,
            record.status
        );
        checked += 1;
    }
    assert!(
        checked >= 2,
        "expected a destroy record per operation, saw {checked}"
    );
}

/// Destroying a workspace must remove its record and its directory, and leave the
/// journal terminal.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn destroying_a_workspace_leaves_no_record_and_a_terminal_journal() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-destroy-workspace");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-destroy-ws-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");

    let workspace_dir = Path::new(&workspace.workspace_path).to_path_buf();
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");

    assert!(
        !workspace_dir.exists(),
        "the workspace directory {} survived the destroy",
        workspace_dir.display()
    );
    let recorded = enclave::registry::with_registry(&state, |registry| {
        Ok(registry
            .sandboxes
            .get(&sandbox.id)
            .map(|sandbox| sandbox.workspaces.contains_key(&workspace.id)))
    })
    .expect("read the registry");
    assert_eq!(
        recorded,
        Some(false),
        "the registry still records the destroyed workspace"
    );
    assert_journal_terminal(&state);

    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

/// Destroying a sandbox must take the rootfs bind mount and the sandbox cgroup with
/// it, not only the directory.
///
/// Both are host state rather than files, so neither is visible in the sandboxes
/// tree once the directory is gone. A destroy that removed the directory and left
/// the bind mount behind would leak a mount the operator can only find by reading
/// mountinfo, and a sandbox cgroup left behind would keep the workspace cgroups that
/// were nested under it.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn destroying_a_sandbox_releases_its_mount_and_cgroup() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-destroy-sandbox");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-destroy-sb-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    let running = start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");

    let mounted_rootfs = running.mounted_rootfs_path.clone();
    let cgroup = sandbox_cgroup_path(&sandbox.id);
    assert!(
        is_mountpoint(&mounted_rootfs),
        "the sandbox rootfs should be mounted while it is running"
    );

    // The last workspace goes first: the sandbox cgroup cannot be removed while a
    // workspace cgroup sits under it.
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");

    assert!(
        !is_mountpoint(&mounted_rootfs),
        "the sandbox rootfs bind mount at {mounted_rootfs} survived the destroy"
    );
    assert!(
        !cgroup.exists(),
        "the sandbox cgroup {} survived the destroy",
        cgroup.display()
    );
    assert!(
        !Path::new(&sandbox.sandbox_path).exists(),
        "the sandbox directory {} survived the destroy",
        sandbox.sandbox_path
    );
    assert_journal_terminal(&state);

    let _ = fs::remove_dir_all(state);
}
