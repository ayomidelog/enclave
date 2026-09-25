//! Recovering the two copies of a workspace record.

use std::fs;
use std::path::Path;

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, destroy_workspace, start_workspace, stop_workspace, WorkspaceLimits,
};

use super::support::{
    prepare_cached_rootfs, process_starttime, root_only, state_dir, workspace_cgroup_path,
};

/// Repair must not delete a workspace whose metadata file is gone while its
/// runtime is still alive.
///
/// The registry and the per-directory metadata are two copies of one record, and
/// repair rebuilds the registry from the tree. A workspace directory with no
/// `workspace.json` therefore looks like an orphan to the scan, but the runtime it
/// left behind is real, and deleting the directory would leave that runtime, its
/// cgroup, its interface, and its firewall rules owned by nothing. The directory is
/// retained and reported instead, and discovery reads only the markers the runtime
/// itself wrote, so it never signals a process to find out.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn repair_retains_a_workspace_whose_metadata_is_gone_but_whose_runtime_lives() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-missing-workspace-metadata");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-missing-metadata-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let runtime_pid = started.runtime_pid.expect("runtime pid");

    // Remove only the per-directory copy. The registry still records the workspace
    // and the runtime is untouched, which is the disagreement repair has to resolve.
    let metadata_path = Path::new(&workspace.workspace_path).join("workspace.json");
    fs::remove_file(&metadata_path).expect("remove the workspace metadata");

    let report = enclave::registry::repair_registry(&state, false).expect("repair the registry");

    assert!(
        Path::new(&workspace.workspace_path).is_dir(),
        "repair deleted the directory of a workspace whose runtime is still alive"
    );
    let retained = report
        .retained_orphans
        .iter()
        .find(|orphan| orphan.workspace_id == workspace.id)
        .unwrap_or_else(|| {
            panic!(
                "repair must report the retained workspace; report was {}",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            )
        });
    assert_eq!(
        retained.runtime.runtime_pid,
        Some(runtime_pid),
        "repair must identify the live runtime from the markers it wrote"
    );
    assert_eq!(
        process_starttime(runtime_pid),
        started.runtime_starttime_ticks,
        "repair signalled the workspace runtime it was supposed to leave alone"
    );

    // The scan skipped this directory, and repair makes the registry describe what
    // the scan found, so the workspace is no longer managed by the registry and this
    // test releases what it created itself.
    unsafe { libc::kill(runtime_pid as i32, libc::SIGKILL) };
    for _ in 0..50 {
        if process_starttime(runtime_pid).is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = fs::remove_dir(workspace_cgroup_path(&sandbox.id, &workspace.id));
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    let _ = destroy_sandbox(&state, &sandbox.id);
    let _ = fs::remove_dir_all(state);
}

/// Repair must rebuild a workspace record that only the registry lost.
///
/// The per-directory metadata is what the scan reads, so a record deleted from the
/// registry is recoverable from the directory it describes. The rebuilt record has
/// to carry the runtime identity the file holds: a record that came back without it
/// would describe a workspace that is running as one that is not, and the next stop
/// would look for nothing to release.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn repair_rebuilds_a_workspace_record_the_registry_lost() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-missing-registry-record");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-missing-record-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");

    enclave::registry::with_registry_mut(&state, |registry| {
        registry
            .sandboxes
            .get_mut(&sandbox.id)
            .expect("sandbox record")
            .workspaces
            .remove(&workspace.id);
        Ok(())
    })
    .expect("drop the workspace record");

    let report = enclave::registry::repair_registry(&state, false).expect("repair the registry");
    assert_eq!(
        report.added_workspaces, 1,
        "repair must report rebuilding exactly the workspace record the registry lost"
    );

    let rebuilt = enclave::registry::with_registry(&state, |registry| {
        Ok(registry
            .sandboxes
            .get(&sandbox.id)
            .and_then(|sandbox| sandbox.workspaces.get(&workspace.id))
            .cloned())
    })
    .expect("read the repaired registry")
    .expect("repair must restore the workspace record from its directory");
    assert_eq!(
        rebuilt.runtime_pid, started.runtime_pid,
        "the rebuilt record must carry the runtime the workspace is actually running"
    );
    assert_eq!(
        rebuilt.runtime_starttime_ticks,
        started.runtime_starttime_ticks
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
