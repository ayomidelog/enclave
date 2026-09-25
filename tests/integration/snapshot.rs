//! Snapshotting a workspace's filesystem and restoring it.
//!
//! A snapshot is only worth having if a restore puts the filesystem back the way
//! it was, so these tests check the contents after a restore rather than only that
//! the commands reported success.

use std::fs;
use std::path::Path;

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, create_workspace_snapshot, destroy_workspace, restore_workspace_snapshot,
    start_workspace, stop_workspace, WorkspaceLimits,
};

use super::support::{prepare_cached_rootfs, root_only, state_dir};

#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn snapshot_restore_recovers_filesystem_state() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-snapshot");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-snapshot-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let workspace = create_workspace(&state, &sandbox.id, "ws", WorkspaceLimits::default())
        .expect("create workspace");

    fs::write(
        Path::new(&workspace.filesystem_path).join("state.txt"),
        "before",
    )
    .expect("seed file");
    create_workspace_snapshot(&state, &sandbox.id, &workspace.id, Some("snap1"))
        .expect("create snapshot");

    fs::write(
        Path::new(&workspace.filesystem_path).join("state.txt"),
        "after",
    )
    .expect("mutate");
    restore_workspace_snapshot(&state, &sandbox.id, &workspace.id, "snap1").expect("restore");

    let restored = fs::read_to_string(Path::new(&workspace.filesystem_path).join("state.txt"))
        .expect("read restored file");
    assert_eq!(restored, "before");

    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

#[test]
#[ignore = "requires root privileges, namespace/mount support, and loopback ext4 mounts"]
fn snapshot_restore_recovers_quota_backed_workspace_state() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-snapshot-disk-quota");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-snapshot-quota-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let limits = WorkspaceLimits {
        disk_bytes: Some(64 * 1024 * 1024),
        ..WorkspaceLimits::default()
    };
    let workspace =
        create_workspace(&state, &sandbox.id, "quota-snap", limits).expect("create workspace");

    fs::write(
        Path::new(&workspace.filesystem_path).join("state.txt"),
        "before",
    )
    .expect("seed file");
    create_workspace_snapshot(&state, &sandbox.id, &workspace.id, Some("snap1"))
        .expect("create snapshot");

    fs::write(
        Path::new(&workspace.filesystem_path).join("state.txt"),
        "after",
    )
    .expect("mutate");
    restore_workspace_snapshot(&state, &sandbox.id, &workspace.id, "snap1").expect("restore");

    start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let restored = fs::read_to_string(Path::new(&workspace.filesystem_path).join("state.txt"))
        .expect("read restored file");
    assert_eq!(restored, "before");

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
