//! What a destructive command must refuse to touch.
//!
//! Enclave releases the host resources it created and proves the release with a
//! certificate. The other half of that guarantee is the resources it must leave
//! alone: a mount the operator placed, or one another tool placed, sitting where
//! Enclave's own tree is. Deleting a workspace directory recurses into whatever is
//! mounted inside it, so the mount has to be recognised as foreign before the
//! directory is removed rather than after.

use std::fs;
use std::path::Path;
use std::process::Command;

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{create_workspace, destroy_workspace, WorkspaceLimits};

use super::support::{prepare_cached_rootfs, root_only, state_dir};

/// Mount a fresh tmpfs at `mountpoint`, standing in for a mount Enclave did not
/// create.
fn mount_foreign_tmpfs(mountpoint: &Path) {
    let status = Command::new("mount")
        .arg("-t")
        .arg("tmpfs")
        .arg("tmpfs")
        .arg(mountpoint)
        .status()
        .expect("run mount");
    assert!(
        status.success(),
        "failed to mount tmpfs at {}",
        mountpoint.display()
    );
}

fn unmount(mountpoint: &Path) {
    let _ = Command::new("umount").arg("-l").arg(mountpoint).status();
}

/// Whether the mount table lists `path` as a mount point.
fn is_mounted(path: &Path) -> bool {
    fs::read_to_string("/proc/self/mountinfo")
        .map(|raw| {
            raw.lines()
                .any(|line| line.split_whitespace().nth(4) == path.to_str())
        })
        .unwrap_or(false)
}

/// A destroy must not delete a workspace through a mount it did not create.
///
/// Removing the workspace directory recurses into everything mounted below it. A
/// mount placed there by the operator therefore has its contents deleted by a
/// command that reported success, which is the worst outcome available: the data
/// is gone and nothing said so. Enclave recognises a mount it did not create,
/// refuses the destroy, and keeps the workspace record so the operator can decide
/// what to do with their mount.
#[test]
#[ignore = "requires root privileges and mount support"]
fn destroy_refuses_to_delete_through_a_mount_enclave_did_not_create() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-foreign-mount");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-foreign-mount-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");

    let foreign = Path::new(&workspace.workspace_path).join("foreign");
    fs::create_dir_all(&foreign).expect("create the foreign mountpoint");
    mount_foreign_tmpfs(&foreign);
    let marker = foreign.join("operator-data");
    fs::write(&marker, "keep me").expect("write into the foreign mount");

    let error = destroy_workspace(&state, &sandbox.id, &workspace.id)
        .expect_err("destroy must refuse a mount it did not create");
    assert!(
        format!("{error:#}").contains("not created by Enclave"),
        "the error must name the mount it refused to touch: {error:#}"
    );

    // Nothing was released: not the mount, not its contents, not the directory.
    assert!(is_mounted(&foreign), "the foreign mount was detached");
    assert_eq!(
        fs::read_to_string(&marker).expect("read the marker file"),
        "keep me",
        "the contents of the foreign mount were deleted"
    );
    assert!(
        Path::new(&workspace.workspace_path).is_dir(),
        "the workspace directory was deleted while a foreign mount was inside it"
    );

    // Once the operator releases their own mount, the same destroy succeeds.
    unmount(&foreign);
    destroy_workspace(&state, &sandbox.id, &workspace.id)
        .expect("destroy must succeed once the foreign mount is gone");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
