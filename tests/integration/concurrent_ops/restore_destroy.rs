//! A snapshot restore racing a destroy on one workspace.
//!
//! A restore rewrites the workspace filesystem from a snapshot and stops the runtime
//! first; a destroy releases the same resources and removes the record. A destroy that
//! reports success must therefore have removed both the record and the directory, and a
//! destroy that is refused must have kept its record, because the record is what names
//! the resources a later repair has to release.

use std::fs;
use std::path::Path;

use enclave::workspace::{
    create_workspace, create_workspace_snapshot, destroy_workspace, list_workspaces,
    restore_workspace_snapshot, start_workspace, stop_workspace, WorkspaceLimits,
};

use super::cached_rootfs_sandbox;
use crate::integration::support::{root_only, state_dir, SandboxCleanup};

/// A snapshot restore racing a destroy must not leave a half-restored filesystem.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_snapshot_restore_racing_a_destroy_does_not_leave_a_partial_workspace() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-restore-destroy-race");
    let mut cleanup = SandboxCleanup::new(state.clone());
    let sandbox_id = cached_rootfs_sandbox(&state, "itest-restore-race-sandbox", &mut cleanup);

    let workspace = create_workspace(&state, &sandbox_id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    start_workspace(&state, &sandbox_id, &workspace.id).expect("start workspace");

    let marker = Path::new(&workspace.filesystem_path).join("state.txt");
    fs::write(&marker, "snapshot").expect("seed the file the snapshot records");
    create_workspace_snapshot(&state, &sandbox_id, &workspace.id, Some("snap1"))
        .expect("create snapshot");
    fs::write(&marker, "after").expect("change the file the snapshot records");

    for round in 0..4 {
        // The file is put back to the state the snapshot does not hold, so a restore
        // that wins is visible in it.
        if marker.exists() {
            fs::write(&marker, "after").expect("reset the file");
        }
        // A restore stops the runtime, so a round that followed one starts from a
        // stopped workspace. Starting it again is what keeps every round racing the
        // same pair of states rather than a stop that has nothing to stop.
        let running = list_workspaces(&state, Some(&sandbox_id))
            .expect("list workspaces")
            .into_iter()
            .find(|item| item.id == workspace.id)
            .expect("the workspace record is gone")
            .runtime_pid
            .is_some();
        if !running {
            start_workspace(&state, &sandbox_id, &workspace.id)
                .expect("restart the workspace before the next round");
        }
        let (restored, destroyed) = std::thread::scope(|scope| {
            let restore = scope
                .spawn(|| restore_workspace_snapshot(&state, &sandbox_id, &workspace.id, "snap1"));
            let destroy = scope.spawn(|| destroy_workspace(&state, &sandbox_id, &workspace.id));
            (
                restore
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                destroy
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            )
        });
        eprintln!(
            "round {round}: restore {}, destroy {}",
            if restored.is_ok() {
                "settled"
            } else {
                "refused"
            },
            if destroyed.is_ok() {
                "settled"
            } else {
                "refused"
            }
        );

        let recorded = list_workspaces(&state, Some(&sandbox_id))
            .expect("list workspaces")
            .into_iter()
            .find(|item| item.id == workspace.id);

        if destroyed.is_ok() {
            // A destroy that succeeded has to have removed the record and the
            // directory, and the restore must not have recreated either. This is the
            // round the race is for: a restore that wrote files back after the destroy
            // removed the directory would leave them behind with nothing describing
            // them.
            assert!(
                recorded.is_none(),
                "round {round}: the destroy succeeded and the record is still there"
            );
            assert!(
                !Path::new(&workspace.workspace_path).exists(),
                "round {round}: the destroy succeeded and {} is still there",
                workspace.workspace_path
            );
            // The workspace is gone, so there is nothing left to race. The test is
            // done with the fixture; the rest of the rounds would have nothing to act
            // on.
            break;
        }

        // A refused destroy keeps its registry record: the record is what names the
        // resources a later repair has to release, so losing it would lose the only
        // description of what is still on the host. The status has to be one of the
        // settled ones, because a transitional status blocks every later operation
        // on the workspace until a repair rolls it back.
        let recorded =
            recorded.expect("the destroy was refused, so its registry record must have been kept");
        assert!(
            matches!(recorded.status.as_str(), "running" | "stopped"),
            "round {round}: the race left the workspace {}",
            recorded.status.as_str()
        );
        // The files may or may not still be there: a normal destroy refuses after the
        // artifact removal when a check that only becomes visible once the files are
        // gone fails, and the retained record is the recovery evidence for exactly
        // that case. What the round does have to leave is a workspace that still
        // works, which the loop checks by starting it before the next round.
        if marker.exists() {
            let content = fs::read_to_string(&marker).expect("read the workspace file");
            assert!(
                content == "snapshot" || content == "after",
                "round {round}: the workspace file holds {content:?}, which is neither state"
            );
        }
    }
    // Whatever the rounds settled on, the workspace is gone or it still starts.
    let recorded = list_workspaces(&state, Some(&sandbox_id))
        .expect("list workspaces")
        .into_iter()
        .find(|item| item.id == workspace.id);
    if recorded.is_some() {
        stop_workspace(&state, &sandbox_id, &workspace.id).expect("stop workspace");
        start_workspace(&state, &sandbox_id, &workspace.id)
            .expect("the workspace must still start after the race");
        destroy_workspace(&state, &sandbox_id, &workspace.id).expect("destroy workspace");
    }

    drop(cleanup);
}
