//! A workspace-wide wipe racing a workspace start.
//!
//! A wipe destroys every workspace in every sandbox, and a start brings one runtime up.
//! They overlap on the workspace directory, its disk image, its cgroup, and its network
//! state, so whichever order they land in the outcome has to be one of two settled ones:
//! the workspace is gone, or it is running and everything that describes it agrees. What
//! must never be left is a workspace caught between the two, and the worst shape of that
//! is a live runtime whose record was removed: the session keeps running with its cgroup,
//! its interface, and its mounts, and nothing left on the host names them.
//!
//! The window is real rather than theoretical. A wipe plans from a snapshot of the
//! registry and then releases each workspace in turn, so a start that commits while the
//! wipe is still working through the others makes the snapshot stale: it describes a
//! stopped workspace while a runtime is now running. The test widens that window on
//! purpose, with one cleanup worker and a queue of workspaces ahead of the target, so the
//! target's turn comes after the start has had time to get well into its launch. What it
//! catches is the workspace directory a start recreates underneath the wipe: the wipe
//! removed the record, the start's launch put the directory back on its way to a runtime
//! the wipe had already decided not to stop, and the record that would have described it
//! is gone. Before the destroy re-resolved and swept the directory again, this failed on
//! every run.

use std::path::Path;

use enclave::workspace::{
    create_workspace, destroy_all_workspaces, list_workspaces, start_workspace, stop_workspace,
    CleanupMode, WorkspaceLimits,
};

use super::cached_rootfs_sandbox;
use crate::integration::support::{
    cgroup_processes, process_starttime, root_only, session_processes_for, state_dir,
    workspace_cgroup_path, SandboxCleanup,
};

/// How many workspaces the wipe has to get through before the one the start is racing.
const QUEUE_AHEAD: usize = 20;

/// A wipe and a start of one workspace, started together, must leave no half-deleted
/// workspace.
///
/// The wipe runs over every workspace, so it holds the global lease in the daemon; a
/// start holds the workspace's. The library only owes the narrower guarantee the registry
/// lock gives it, which is that the mutations are ordered rather than the host work, so
/// this drives the pair directly and checks what the host looks like afterwards.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_wipe_racing_a_workspace_start_leaves_no_half_deleted_workspace() {
    if !root_only() {
        return;
    }

    // One cleanup worker, so the wipe releases the workspaces strictly one at a time and
    // the target's turn is far enough behind the plan for a start to commit in between.
    // The variable is read by the wipe itself rather than by the daemon, and the
    // privileged suite runs one test at a time, so nothing else observes it.
    std::env::set_var("ENCLAVE_CLEANUP_WORKERS", "1");

    let state = state_dir("enclave-int-wipe-start-race");
    let mut cleanup = SandboxCleanup::new(state.clone());
    let sandbox_id = cached_rootfs_sandbox(&state, "itest-wipe-race-sandbox", &mut cleanup);

    // The workspaces ahead of the target in the queue. Their ids sort before the
    // target's, and the wipe walks the registry in id order, so they are released first.
    for index in 0..QUEUE_AHEAD {
        create_workspace(
            &state,
            &sandbox_id,
            &format!("ahead{index}"),
            WorkspaceLimits::default(),
        )
        .expect("create a workspace ahead of the target");
    }
    let target = create_workspace(&state, &sandbox_id, "target", WorkspaceLimits::default())
        .expect("create the target workspace");
    // `workspace create` starts what it creates, and a start of a workspace that is
    // already running is answered from the live runtime without launching. The race is
    // about a start that really launches, so the target is stopped first.
    stop_workspace(&state, &sandbox_id, &target.id)
        .expect("stop the target so the racing start has to launch it");

    let (wiped, started) = std::thread::scope(|scope| {
        let wipe = scope.spawn(|| destroy_all_workspaces(&state, CleanupMode::Force));
        let start = scope.spawn(|| start_workspace(&state, &sandbox_id, &target.id));
        (
            wipe.join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            start
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
        )
    });
    eprintln!(
        "wipe {}, start {}",
        match &wiped {
            Ok(report) => format!(
                "settled ({} removed, {} error(s))",
                report.removed.len(),
                report.errors.len()
            ),
            Err(error) => format!("refused: {error:#}"),
        },
        match &started {
            Ok(_) => "settled".to_string(),
            Err(error) => format!("refused: {error:#}"),
        }
    );

    assert_wipe_race_is_settled(&state, &sandbox_id, &target.id, &target.workspace_path);

    // The pair has to leave the sandbox usable, so a fresh workspace still starts.
    let restarted = create_workspace(&state, &sandbox_id, "restarted", WorkspaceLimits::default())
        .expect("create a workspace after the race");
    start_workspace(&state, &sandbox_id, &restarted.id)
        .expect("the sandbox must still start a workspace");
    destroy_all_workspaces(&state, CleanupMode::Force).expect("clear the workspaces");
    std::env::remove_var("ENCLAVE_CLEANUP_WORKERS");
    drop(cleanup);
}

/// No workspace may be left half deleted, and no runtime may be left unnamed.
///
/// The check is about agreement rather than about which of the two won. Every workspace
/// still in the registry has to have a directory that matches its status, and every
/// workspace the wipe removed has to have left nothing of its own on the host. The second
/// half is what catches the failure this test exists for: a runtime that outlives the
/// removal of the record describing it.
fn assert_wipe_race_is_settled(state: &Path, sandbox_id: &str, target_id: &str, target_path: &str) {
    let records = list_workspaces(state, Some(sandbox_id)).expect("list workspaces");
    for record in &records {
        let directory_exists = Path::new(&record.workspace_path).is_dir();
        assert!(
            directory_exists,
            "the record of {} survived but its directory did not",
            record.id
        );
        match record.status.as_str() {
            "running" => {
                let pid = record
                    .runtime_pid
                    .unwrap_or_else(|| panic!("{} is running without a pid", record.id));
                assert_eq!(
                    process_starttime(pid),
                    record.runtime_starttime_ticks,
                    "{} names a process that is not the one running",
                    record.id
                );
                let cgroup = workspace_cgroup_path(sandbox_id, &record.id);
                assert!(
                    cgroup_processes(&cgroup)
                        .iter()
                        .any(|(held, _)| *held == pid),
                    "{} is running but its runtime is not in its cgroup",
                    record.id
                );
            }
            "stopped" => {
                assert_eq!(
                    record.runtime_pid, None,
                    "{} is stopped with a pid",
                    record.id
                );
                assert!(!workspace_cgroup_path(sandbox_id, &record.id).exists());
            }
            other => panic!("the race left {} {other}", record.id),
        }
    }

    // The target's own record may be gone, which is the wipe winning. What it may not
    // leave behind is anything of its own: a session, a cgroup, or the directory.
    if !records.iter().any(|record| record.id == target_id) {
        assert!(
            !Path::new(target_path).is_dir(),
            "the wipe removed the target's record but left {target_path} behind"
        );
        assert!(
            !workspace_cgroup_path(sandbox_id, target_id).exists(),
            "the wipe removed the target's record but left its cgroup behind"
        );
        let orphans = session_processes_for(Path::new(target_path));
        assert!(
            orphans.is_empty(),
            "the wipe removed the target's record but left its runtime running: {orphans:?}"
        );
    }
}
