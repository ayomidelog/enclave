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
    prepare_cached_rootfs, process_starttime, root_only, sandbox_dir, state_dir,
    workspace_cgroup_path, workspace_dir, TestDaemon,
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

/// A repair has to resolve an interrupted transition, not only report it.
///
/// A launch writes the record `starting` before it does any host work, so a daemon
/// killed during one leaves a record describing an operation nobody is running. The
/// read-only doctor names it, which is what an operator sees first, but naming it is
/// not fixing it: the record stays `starting` and the same finding comes back on every
/// run. Repair is the command that is asked to make the records describe the host, so
/// the rollback belongs to it as much as to the next daemon start.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn doctor_repair_rolls_back_a_workspace_left_mid_transition() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-doctor-repair-transition");
    prepare_cached_rootfs(&state, "bookworm");
    let socket_dir = std::env::temp_dir().join(format!(
        "enclave-int-doctor-repair-transition-socket-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&socket_dir);
    fs::create_dir_all(&socket_dir).expect("create the socket directory");

    let mut daemon = TestDaemon::new(&state, &socket_dir);
    daemon.start();
    daemon.cli_ok(&[
        "create",
        "repairbox",
        "--suite",
        "bookworm",
        "--bootstrap-method",
        "cached_rootfs",
    ]);
    daemon.cli_ok(&["workspace", "create", "repairbox", "dev"]);
    daemon.cli_ok(&["workspace", "stop", "repairbox", "dev"]);

    let sandbox_dir = sandbox_dir(&state, "repairbox");
    let workspace_dir = workspace_dir(&sandbox_dir, "dev");

    // Leave the record mid-transition the way an interrupted launch does, with the
    // address already reserved. Both copies are written, because repair adopts the
    // on-disk one and a disagreement would be resolved before the rollback ran.
    for path in [
        state.join("registry.json"),
        workspace_dir.join("workspace.json"),
    ] {
        let raw = fs::read_to_string(&path).expect("read the record");
        let rewritten = rewrite_workspace_status(&raw, "starting");
        fs::write(&path, rewritten).expect("write the record");
    }

    // The read-only doctor sees it, which is the report an operator gets first.
    let before = daemon.cli(&["workspace", "status", "repairbox", "dev"]);
    assert!(
        String::from_utf8_lossy(&before.stdout).contains("status: starting"),
        "the fixture has to leave the workspace transitional"
    );

    let repaired = daemon.cli(&["doctor", "--repair"]);
    assert!(
        repaired.status.success(),
        "doctor --repair failed: {}",
        String::from_utf8_lossy(&repaired.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&repaired.stdout).expect("parse the repair report");
    assert_eq!(
        report["reconciled_runtime_records"].as_u64(),
        Some(1),
        "repair must roll the interrupted transition back, not only report it: {report}"
    );

    let after = daemon.cli(&["workspace", "status", "repairbox", "dev"]);
    assert!(
        String::from_utf8_lossy(&after.stdout).contains("status: stopped"),
        "the workspace is not settled after repair: {}",
        String::from_utf8_lossy(&after.stdout)
    );

    // Recovery has to be idempotent: the record is now one repair has nothing to do
    // with, so a second run settles nothing and a third would be the same. Without
    // this the test would pass on a repair that rolled the workspace back but left
    // the record in a shape the next run would roll back again.
    let second = daemon.cli(&["doctor", "--repair"]);
    assert!(
        second.status.success(),
        "a second repair failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&second.stdout).expect("parse the second repair report");
    assert_eq!(
        report["reconciled_runtime_records"].as_u64(),
        Some(0),
        "a repair with nothing to recover still reported work: {report}"
    );

    daemon.cli_ok(&["destroy", "--force", "repairbox"]);
    drop(daemon);
    let _ = fs::remove_dir_all(&state);
    let _ = fs::remove_dir_all(&socket_dir);
}

/// Rewrite the `status` field of a workspace record, in either of its two shapes.
///
/// The registry nests the workspace under its sandbox; the on-disk copy is the
/// workspace itself. Only the status is changed, so the rest of the record stays
/// exactly as the daemon wrote it.
fn rewrite_workspace_status(raw: &str, status: &str) -> String {
    let mut value: serde_json::Value = serde_json::from_str(raw).expect("parse the record");
    let target = match value.get_mut("sandboxes").and_then(|v| v.as_object_mut()) {
        Some(sandboxes) => sandboxes
            .values_mut()
            .find_map(|sandbox| sandbox.get_mut("workspaces")?.as_object_mut())
            .and_then(|workspaces| workspaces.values_mut().next())
            .expect("the registry holds the workspace"),
        None => &mut value,
    };
    target["status"] = serde_json::Value::String(status.to_string());
    serde_json::to_string_pretty(&value).expect("serialize the record")
}
