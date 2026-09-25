//! The storage a quota-backed workspace gets, and releasing it.
//!
//! A workspace with a disk quota owns an ext4 image on a loop device, and that is
//! the tier whose storage is a real filesystem rather than a directory. These
//! tests check the quota is enforced, that the workspace `/tmp` lives inside the
//! image, that a resize grows it, and that a stop releases the loop device.

use std::fs;
use std::path::Path;

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, destroy_workspace, list_workspaces, resize_workspace_disk, start_workspace,
    stop_workspace, WorkspaceLimits,
};

use super::support::{
    ext4_filesystem_size, loop_devices_backing, mounts_at_or_below, prepare_cached_rootfs,
    root_only, state_dir,
};

#[test]
#[ignore = "requires root privileges, namespace/mount support, and loopback ext4 mounts"]
fn workspace_disk_resize_grows_running_managed_storage() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-disk-resize");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-disk-resize-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let initial_bytes = 64 * 1024 * 1024;
    let expanded_bytes = 96 * 1024 * 1024;
    let workspace = create_workspace(
        &state,
        &sandbox.id,
        "resize",
        WorkspaceLimits {
            disk_bytes: Some(initial_bytes),
            ..WorkspaceLimits::default()
        },
    )
    .expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let old_pid = started.runtime_pid.expect("runtime pid");
    let workspace_file = Path::new("/proc")
        .join(old_pid.to_string())
        .join("root/home/preserved.txt");
    fs::write(&workspace_file, "preserve me").expect("write workspace data");

    let result = resize_workspace_disk(&state, &sandbox.id, &workspace.id, expanded_bytes)
        .expect("resize workspace");
    assert!(result.restarted);
    assert_eq!(result.previous_disk_bytes, initial_bytes);
    assert_eq!(result.new_disk_bytes, expanded_bytes);

    let workspaces = list_workspaces(&state, Some(&sandbox.id)).expect("list workspaces");
    let resized = workspaces
        .into_iter()
        .find(|item| item.id == workspace.id)
        .expect("resized workspace metadata");
    assert_eq!(resized.limits.disk_bytes, Some(expanded_bytes));
    let new_pid = resized.runtime_pid.expect("restarted runtime pid");
    assert_ne!(new_pid, old_pid);
    assert_eq!(
        fs::read_to_string(
            Path::new("/proc")
                .join(new_pid.to_string())
                .join("root/home/preserved.txt")
        )
        .expect("read preserved workspace data"),
        "preserve me"
    );
    assert_eq!(
        fs::metadata(Path::new(&workspace.workspace_path).join("fs.img"))
            .expect("disk image metadata")
            .len(),
        expanded_bytes
    );
    // The image file and the ext4 filesystem inside it must agree. A resize that
    // grows only the image leaves the workspace failing its readiness checks.
    let filesystem_bytes =
        ext4_filesystem_size(&Path::new(&workspace.workspace_path).join("fs.img"))
            .expect("read ext4 filesystem size");
    assert!(
        filesystem_bytes >= expanded_bytes,
        "ext4 filesystem is {filesystem_bytes} bytes, below the requested {expanded_bytes}"
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    let stopped_result = resize_workspace_disk(&state, &sandbox.id, &workspace.id, expanded_bytes)
        .expect("equal stopped resize");
    assert!(!stopped_result.restarted);
    assert!(list_workspaces(&state, Some(&sandbox.id))
        .expect("list stopped workspace")
        .into_iter()
        .find(|item| item.id == workspace.id)
        .expect("stopped workspace metadata")
        .runtime_pid
        .is_none());

    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

/// A quota-backed workspace is a disk image attached to a loop device. A stop has
/// to release that device, or every stop-and-start cycle consumes one: the host
/// has a finite number of them, and a device left attached to a deleted image
/// cannot be recovered without host-side cleanup.
#[test]
#[ignore = "requires root privileges, namespace/mount support, and loopback ext4 mounts"]
fn stopping_a_quota_workspace_releases_its_loop_device() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-loop-release");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-loop-release-sandbox",
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
        create_workspace(&state, &sandbox.id, "quota", limits).expect("create workspace");
    start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");

    let image = Path::new(&workspace.workspace_path).join("fs.img");
    // The mounts are the other half of what a quota-backed workspace owns: the image is
    // mounted, and the root overlay on top of it is merged. A stop that released the loop
    // device but left a mount would leave the image reachable through the host tree, and
    // the loop device check below would not notice because the device is still there for
    // the mount to hold.
    let workspace_path = Path::new(&workspace.workspace_path);
    let mounted = mounts_at_or_below(workspace_path);
    assert!(
        !mounted.is_empty(),
        "a running quota-backed workspace must have its storage mounted: {}",
        workspace_path.display()
    );
    let attached = loop_devices_backing(&image);
    assert!(
        !attached.is_empty(),
        "a running quota-backed workspace must have its image attached to a loop device"
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");

    let survivors = loop_devices_backing(&image);
    assert!(
        survivors.is_empty(),
        "stop left loop device(s) {survivors:?} attached to {}",
        image.display()
    );
    let surviving_mounts = mounts_at_or_below(workspace_path);
    assert!(
        surviving_mounts.is_empty(),
        "stop left mount(s) {surviving_mounts:?} below {}",
        workspace_path.display()
    );

    // A second cycle proves the device was released rather than reused, which is
    // what a device left attached would look like from the outside.
    start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace again");
    assert!(
        !loop_devices_backing(&image).is_empty(),
        "the restarted workspace must attach its image to a loop device again"
    );
    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace again");
    assert!(
        loop_devices_backing(&image).is_empty(),
        "the second stop left a loop device attached to {}",
        image.display()
    );
    assert!(
        mounts_at_or_below(workspace_path).is_empty(),
        "the second stop left a mount below {}",
        workspace_path.display()
    );

    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
