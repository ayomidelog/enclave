//! Reading the mount table, and deciding which mounts are Enclave owns.
//!
//! A cleanup refuses to touch a mount it cannot attribute to itself, so what these
//! pin is the ownership rule rather than the parsing.

use super::*;

#[test]
fn mountinfo_snapshot_orders_nested_mounts_deepest_first() {
    let snapshot = MountInfoSnapshot::parse(concat!(
        "100 1 0:50 / /tmp/enclave/ws/fs rw,relatime - ext4 /dev/loop0 rw\n",
        "101 100 0:51 / /tmp/enclave/ws/fs/cache\\040data rw,relatime - tmpfs tmpfs rw\n"
    ));
    assert_eq!(
        snapshot.at_or_below(Path::new("/tmp/enclave/ws/fs")),
        vec![
            PathBuf::from("/tmp/enclave/ws/fs/cache data"),
            PathBuf::from("/tmp/enclave/ws/fs")
        ]
    );
    assert!(snapshot.contains(Path::new("/tmp/enclave/ws/fs")));
}

#[test]
fn mountinfo_entry_keeps_root_filesystem_and_source() {
    // A bind mount of the sandbox rootfs: the mount root records where it was
    // bound from, and the source is only the underlying device.
    let parsed = entry(concat!(
        "49 29 8:1 /root/.local/state/enclave/sandboxes/sb/rootfs ",
        "/root/.local/state/enclave/sandboxes/sb/runtime/rootfs.mnt ",
        "rw,relatime - ext4 /dev/sda1 rw,discard,errors=remount-ro\n"
    ));
    assert_eq!(
        parsed.root,
        PathBuf::from("/root/.local/state/enclave/sandboxes/sb/rootfs")
    );
    assert_eq!(
        parsed.mountpoint,
        PathBuf::from("/root/.local/state/enclave/sandboxes/sb/runtime/rootfs.mnt")
    );
    assert_eq!(parsed.filesystem_type, "ext4");
    assert_eq!(parsed.source, PathBuf::from("/dev/sda1"));
}

#[test]
fn mountinfo_entry_unescapes_the_root_as_well_as_the_mount_point() {
    let parsed = entry(concat!(
        "50 29 0:60 /a\\040b /mnt/target rw - tmpfs tmpfs rw\n"
    ));
    assert_eq!(parsed.root, PathBuf::from("/a b"));
    assert_eq!(parsed.mountpoint, PathBuf::from("/mnt/target"));
}

#[test]
fn mountinfo_snapshot_ignores_lines_without_a_separator() {
    let snapshot = MountInfoSnapshot::parse("not a mountinfo line\n\n");
    assert!(snapshot.at_or_below_entries(Path::new("/")).is_empty());
    assert!(!snapshot.contains(Path::new("/")));
}

#[test]
fn enclave_state_root_anchors_every_path_enclave_manages() {
    let state = PathBuf::from("/root/.local/state/enclave");
    for path in [
        "/root/.local/state/enclave/sandboxes",
        "/root/.local/state/enclave/sandboxes/sb-1",
        "/root/.local/state/enclave/sandboxes/sb-1/workspaces",
        "/root/.local/state/enclave/sandboxes/sb-1/workspaces/ws-1",
        "/root/.local/state/enclave/sandboxes/sb-1/workspaces/ws-1/fs",
    ] {
        assert_eq!(
            enclave_state_root(Path::new(path)),
            Some(state.clone()),
            "{path} should resolve to the state directory"
        );
    }
    // A path that is not below a `sandboxes` directory has no provable owner, so
    // nothing can be attributed to Enclave.
    assert_eq!(enclave_state_root(Path::new("/tmp/elsewhere/ws")), None);
    assert_eq!(enclave_state_root(Path::new("/")), None);
}

#[test]
fn enclave_owns_overlays_binds_from_the_state_directory_and_its_own_images() {
    let state_dir = Path::new("/root/.local/state/enclave");

    // The sandbox rootfs overlay and a workspace home overlay.
    for line in [
        "100 29 0:50 / /root/.local/state/enclave/sandboxes/sb/rootfs rw - overlay overlay rw\n",
        "101 29 0:51 / /root/.local/state/enclave/sandboxes/sb/workspaces/ws/home-merged rw - overlay overlay rw\n",
    ] {
        assert!(
            entry(line).is_enclave_owned(state_dir),
            "an overlay below the state directory is Enclave's: {line}"
        );
    }

    // A bind of a directory inside the state tree, which is how the workspace
    // source and the sandbox rootfs bind mount are made.
    let bind = entry(concat!(
        "102 29 8:1 /root/.local/state/enclave/sandboxes/sb/workspaces/ws/fs ",
        "/root/.local/state/enclave/sandboxes/sb/workspaces/ws/fs ",
        "rw,relatime - ext4 /dev/sda1 rw\n"
    ));
    assert!(bind.is_enclave_owned(state_dir));
}

#[test]
fn enclave_refuses_a_mount_it_cannot_attribute_to_itself() {
    let state_dir = Path::new("/root/.local/state/enclave");

    // An operator's tmpfs placed under a workspace path.
    let foreign = entry(concat!(
        "200 29 0:60 / /root/.local/state/enclave/sandboxes/sb/workspaces/ws/foreign ",
        "rw,nosuid,nodev - tmpfs tmpfs rw,size=1024k\n"
    ));
    assert!(!foreign.is_enclave_owned(state_dir));

    // A host device mounted at the filesystem root, so its mount root is `/`
    // rather than a directory inside the state tree.
    let device = entry(concat!(
        "201 29 8:1 / /root/.local/state/enclave/sandboxes/sb/workspaces/ws/backup ",
        "rw,relatime - ext4 /dev/sda1 rw\n"
    ));
    assert!(!device.is_enclave_owned(state_dir));

    // A loop device whose backing file cannot be read: an unattributable mount
    // is never treated as Enclave's own.
    let unknown_loop = entry(concat!(
        "202 29 7:999 / /root/.local/state/enclave/sandboxes/sb/workspaces/ws/fs ",
        "rw,relatime - ext4 /dev/loop999 rw\n"
    ));
    assert!(!unknown_loop.is_enclave_owned(state_dir));
}

#[test]
fn mount_snapshot_splits_owned_and_foreign_mounts_below_a_path() {
    let workspace = "/root/.local/state/enclave/sandboxes/sb/workspaces/ws";
    let snapshot = MountInfoSnapshot::parse(&format!(
        concat!(
            "100 29 0:50 / {ws}/home-merged rw - overlay overlay rw\n",
            "200 29 0:60 / {ws}/operator-backup rw - tmpfs tmpfs rw\n",
            "201 29 8:1 / {ws}/operator-disk rw - ext4 /dev/sdb1 rw\n"
        ),
        ws = workspace
    ));

    let owned = snapshot.owned_at_or_below(Path::new(workspace));
    let foreign = snapshot.foreign_at_or_below(Path::new(workspace));
    assert_eq!(owned.len(), 1, "only the overlay is Enclave's: {owned:?}");
    assert_eq!(
        foreign.len(),
        2,
        "both operator mounts are foreign: {foreign:?}"
    );
    assert!(
        foreign.iter().all(|mount| mount.contains("(source ")),
        "a foreign mount is reported with its source: {foreign:?}"
    );
}
