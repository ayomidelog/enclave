//! What two lifecycle operations racing on one workspace are allowed to leave behind.
//!
//! The daemon serializes these pairs with a workspace lease, so the library only owes
//! callers the narrower guarantee the registry lock gives it: the mutations are
//! ordered, the host work is not. A resize stops the runtime, grows the image, grows
//! the filesystem inside it, and starts the runtime again, and a restore rewrites the
//! workspace filesystem from a snapshot. Both overlap a stop and a destroy in host
//! work they cannot order, and the point of these tests is that neither pair can
//! leave the workspace describing something the host does not have.
//!
//! The two are tested together because the guarantee is the same one, and because the
//! invariant each has to preserve is the same shape: whatever the two operations
//! settled on, the record, the image, and the filesystem inside it have to agree.

use std::fs;
use std::path::Path;

use enclave::sandbox::{create_sandbox, start_sandbox, BootstrapMethod};
use enclave::workspace::{
    create_workspace, create_workspace_snapshot, destroy_workspace, list_workspaces,
    resize_workspace_disk, restore_workspace_snapshot, start_workspace, stop_workspace,
    WorkspaceLimits,
};

use super::support::{
    ext4_filesystem_size, loop_devices_backing, prepare_cached_rootfs, root_only, state_dir,
    SandboxCleanup,
};

/// The size of the workspace image on disk, and the size of the filesystem inside it.
///
/// The two are separate facts. Growing the image is a truncate and cannot fail; growing
/// the filesystem inside it is a resize of a live ext4 and can. A workspace left with an
/// image larger than its filesystem reports readiness failures on every later start,
/// which is the failure this pair of numbers exists to catch.
fn image_and_filesystem_bytes(workspace_path: &str) -> (u64, u64) {
    let image = Path::new(workspace_path).join("fs.img");
    let image_bytes = fs::metadata(&image)
        .expect("read the disk image metadata")
        .len();
    let filesystem_bytes = ext4_filesystem_size(&image).expect("read the ext4 filesystem size");
    (image_bytes, filesystem_bytes)
}

/// A sandbox with one quota-backed workspace, which is the tier a resize acts on.
fn quota_sandbox(state: &Path, name: &str, cleanup: &mut SandboxCleanup) -> String {
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
    cleanup.record(&sandbox.id);
    start_sandbox(state, &sandbox.id).expect("start sandbox");
    sandbox.id
}

/// A resize racing a stop must leave the image and the filesystem inside it agreeing.
#[test]
#[ignore = "requires root privileges, namespace/mount support, and loopback ext4 mounts"]
fn a_resize_racing_a_stop_leaves_the_image_and_its_filesystem_agreeing() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-resize-stop-race");
    let mut cleanup = SandboxCleanup::new(state.clone());
    let sandbox_id = quota_sandbox(&state, "itest-resize-race-sandbox", &mut cleanup);

    let initial_bytes = 64 * 1024 * 1024;
    let expanded_bytes = 128 * 1024 * 1024;
    let workspace = create_workspace(
        &state,
        &sandbox_id,
        "resize",
        WorkspaceLimits {
            disk_bytes: Some(initial_bytes),
            ..WorkspaceLimits::default()
        },
    )
    .expect("create workspace");
    start_workspace(&state, &sandbox_id, &workspace.id).expect("start workspace");

    // A handful of rounds, because which of the two settles is decided by the
    // scheduler and one round only ever shows one of the orders. The fixture is one
    // workspace, so this stays cheap.
    for round in 0..4 {
        let (resized, stopped) = std::thread::scope(|scope| {
            let resize = scope.spawn(|| {
                resize_workspace_disk(&state, &sandbox_id, &workspace.id, expanded_bytes)
            });
            let stop = scope.spawn(|| stop_workspace(&state, &sandbox_id, &workspace.id));
            (
                resize
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                stop.join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            )
        });
        eprintln!(
            "round {round}: resize {}, stop {}",
            if resized.is_ok() {
                "settled"
            } else {
                "refused"
            },
            if stopped.is_ok() {
                "settled"
            } else {
                "refused"
            }
        );

        // Whatever the pair settled on, the image and the filesystem inside it have to
        // agree. A resize that grew the image and was then interrupted by the stop
        // would leave the two disagreeing, and the record would name a size the
        // filesystem does not have.
        let (image_bytes, filesystem_bytes) = image_and_filesystem_bytes(&workspace.workspace_path);
        assert!(
            filesystem_bytes >= image_bytes,
            "round {round}: the image is {image_bytes} bytes and the filesystem inside it is {filesystem_bytes}"
        );
        assert!(
            image_bytes == initial_bytes || image_bytes == expanded_bytes,
            "round {round}: the image is {image_bytes} bytes, which is neither the original {initial_bytes} nor the requested {expanded_bytes}"
        );

        // The record's declared size is what the next start reads, so it has to be one
        // of the two sizes the image is allowed to have as well.
        let recorded = list_workspaces(&state, Some(&sandbox_id))
            .expect("list workspaces")
            .into_iter()
            .find(|item| item.id == workspace.id)
            .expect("the test's workspace record is gone");
        assert!(
            recorded.limits.disk_bytes == Some(initial_bytes)
                || recorded.limits.disk_bytes == Some(expanded_bytes),
            "round {round}: the record declares {} bytes",
            recorded.limits.disk_bytes.unwrap_or(0)
        );
        assert!(
            recorded.limits.disk_bytes == Some(image_bytes),
            "round {round}: the record declares {} bytes while the image is {image_bytes}",
            recorded.limits.disk_bytes.unwrap_or(0)
        );

        // The race must not leave the workspace unusable, so each round ends with it
        // running and the next round starts from a settled state.
        if recorded.runtime_pid.is_none() {
            start_workspace(&state, &sandbox_id, &workspace.id).expect("restart after the race");
        }
    }

    // A final settle: the workspace stops cleanly, releases its loop device, and starts
    // again with the filesystem it now has.
    stop_workspace(&state, &sandbox_id, &workspace.id).expect("stop workspace");
    assert!(
        loop_devices_backing(&Path::new(&workspace.workspace_path).join("fs.img")).is_empty(),
        "the stop left the image attached to a loop device"
    );
    let (image_bytes, filesystem_bytes) = image_and_filesystem_bytes(&workspace.workspace_path);
    assert!(
        filesystem_bytes >= image_bytes,
        "the image is {image_bytes} bytes and the filesystem inside it is {filesystem_bytes}"
    );
    start_workspace(&state, &sandbox_id, &workspace.id).expect("the workspace must still start");

    destroy_workspace(&state, &sandbox_id, &workspace.id).expect("destroy workspace");
    drop(cleanup);
}

/// A snapshot restore racing a destroy must not leave a half-restored filesystem.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_snapshot_restore_racing_a_destroy_does_not_leave_a_partial_workspace() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-restore-destroy-race");
    let mut cleanup = SandboxCleanup::new(state.clone());
    let sandbox_id = quota_sandbox(&state, "itest-restore-race-sandbox", &mut cleanup);

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
