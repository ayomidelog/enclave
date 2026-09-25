//! A resize racing a stop on one quota-backed workspace.
//!
//! A resize stops the runtime, grows the image, grows the filesystem inside it, and
//! starts the runtime again. A stop reaches into the same three things, so the pair
//! overlaps in host work they cannot order. The invariant is that the image and the
//! filesystem inside it still agree, because a workspace whose image is larger than its
//! filesystem fails its readiness checks on every later start.

use std::path::Path;

use enclave::workspace::{
    create_workspace, destroy_workspace, list_workspaces, resize_workspace_disk, start_workspace,
    stop_workspace, WorkspaceLimits,
};

use super::{cached_rootfs_sandbox, image_and_filesystem_bytes};
use crate::integration::support::{loop_devices_backing, root_only, state_dir, SandboxCleanup};

/// A resize racing a stop must leave the image and the filesystem inside it agreeing.
#[test]
#[ignore = "requires root privileges, namespace/mount support, and loopback ext4 mounts"]
fn a_resize_racing_a_stop_leaves_the_image_and_its_filesystem_agreeing() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-resize-stop-race");
    let mut cleanup = SandboxCleanup::new(state.clone());
    let sandbox_id = cached_rootfs_sandbox(&state, "itest-resize-race-sandbox", &mut cleanup);

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
