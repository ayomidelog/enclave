use std::path::PathBuf;

use super::*;

fn loopback_fixture_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("enclave-loopback-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_backing(sys_block: &std::path::Path, name: &str, backing: &str) {
    let loop_dir = sys_block.join(name).join("loop");
    fs::create_dir_all(&loop_dir).unwrap();
    fs::write(loop_dir.join("backing_file"), backing).unwrap();
}

#[test]
fn sysfs_loop_scan_reads_attached_backings_and_skips_free_devices() {
    let sys_block = loopback_fixture_dir("scan");
    write_backing(&sys_block, "loop0", "/tmp/a/fs.img\n");
    // An unattached device has the directory but no backing file.
    fs::create_dir_all(sys_block.join("loop1/loop")).unwrap();
    write_backing(&sys_block, "loop2", "/tmp/b/fs.img");
    // A non-loop block device is not a loop device.
    write_backing(&sys_block, "sda", "/tmp/ignored");

    let devices = sysfs_loop_devices_in(&sys_block).unwrap();
    assert_eq!(
        devices,
        vec![
            LoopDevice {
                device: "/dev/loop0".to_string(),
                backing: PathBuf::from("/tmp/a/fs.img"),
            },
            LoopDevice {
                device: "/dev/loop2".to_string(),
                backing: PathBuf::from("/tmp/b/fs.img"),
            },
        ]
    );
    let _ = fs::remove_dir_all(&sys_block);
}

#[test]
fn sysfs_loop_scan_orders_by_device_index_and_reports_missing_sysfs() {
    let sys_block = loopback_fixture_dir("order");
    write_backing(&sys_block, "loop10", "/tmp/ten");
    write_backing(&sys_block, "loop2", "/tmp/two");

    let devices = sysfs_loop_devices_in(&sys_block).unwrap();
    assert_eq!(
        devices
            .iter()
            .map(|d| d.device.as_str())
            .collect::<Vec<_>>(),
        vec!["/dev/loop2", "/dev/loop10"]
    );
    assert!(sysfs_loop_devices_in(&sys_block.join("absent")).is_none());
    let _ = fs::remove_dir_all(&sys_block);
}

#[test]
fn loop_device_selection_matches_the_recorded_backing() {
    let devices = vec![
        LoopDevice {
            device: "/dev/loop0".to_string(),
            backing: PathBuf::from("/tmp/one/fs.img"),
        },
        LoopDevice {
            device: "/dev/loop1".to_string(),
            backing: PathBuf::from("/tmp/two/fs.img"),
        },
    ];

    assert_eq!(
        select_backing(&devices, std::path::Path::new("/tmp/two/fs.img")),
        vec!["/dev/loop1"]
    );
    assert!(select_backing(&devices, std::path::Path::new("/tmp/absent/fs.img")).is_empty());
}

#[test]
fn write_file_atomic_creates_file_with_correct_content() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-test-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.txt");

    write_file_atomic(&path, b"hello world", 0o600).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "hello world");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn write_file_atomic_sets_permissions() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-perms-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("perms.txt");

    write_file_atomic(&path, b"content", 0o600).unwrap();

    let metadata = fs::metadata(&path).unwrap();
    let mode = metadata.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn write_file_atomic_overwrites_existing() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-overwrite-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("overwrite.txt");

    write_file_atomic(&path, b"first", 0o600).unwrap();
    write_file_atomic(&path, b"second", 0o600).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "second");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn with_file_lock_executes_operation() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-lock-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let lock_path = dir.join("test.lock");

    let result = with_file_lock(&lock_path, || Ok(42)).unwrap();
    assert_eq!(result, 42);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn with_file_lock_propagates_errors() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-lockerr-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let lock_path = dir.join("test.lock");

    let result: Result<()> = with_file_lock(&lock_path, || anyhow::bail!("intentional error"));
    assert!(result.is_err());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ensure_secure_dir_creates_and_validates_directory() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-secure-{}", std::process::id()));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }

    ensure_secure_dir(&dir).unwrap();

    assert!(dir.is_dir());
    let metadata = fs::metadata(&dir).unwrap();
    let mode = metadata.permissions().mode() & 0o777;
    assert_eq!(
        mode & 0o022,
        0,
        "directory should not be group/world writable"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ensure_secure_dir_rejects_sticky_world_writable_mode() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-sticky-{}", std::process::id()));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o1777)).unwrap();

    let err = ensure_secure_dir(&dir).unwrap_err();
    assert!(
        err.to_string()
            .contains("use a private subdirectory (0700) instead"),
        "unexpected error: {err:#}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ensure_path_within_rejects_traversal() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-within-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();

    let result = ensure_path_within(&dir, &PathBuf::from("../../etc/passwd"), "test_path");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("escapes base directory"), "got: {}", msg);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ensure_path_within_accepts_valid_subpath() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-within-ok-{}", std::process::id()));
    fs::create_dir_all(dir.join("sub")).unwrap();

    let result = ensure_path_within(&dir, &PathBuf::from("sub/file.txt"), "test_path");
    assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn temporary_path_for_generates_unique_paths() {
    let path = PathBuf::from("/tmp/myfile.txt");
    let tmp1 = temporary_path_for(&path);
    let tmp2 = temporary_path_for(&path);
    assert_ne!(tmp1, tmp2, "temporary paths should be unique");
    assert!(tmp1.to_string_lossy().contains(".myfile.txt.tmp."));
}

#[test]
fn is_mountpoint_uses_mountinfo_without_mountpoint_process() {
    assert!(is_mountpoint(Path::new("/proc")).expect("read mountinfo"));
    assert!(!is_mountpoint(Path::new("/tmp/enclave-no-such-mount")).expect("read mountinfo"));
}

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

/// Parse one mountinfo line into the entry the ownership rules read.
fn entry(line: &str) -> MountInfoEntry {
    MountInfoSnapshot::parse(line)
        .at_or_below_entries(Path::new("/"))
        .into_iter()
        .next()
        .cloned()
        .expect("a well-formed mountinfo line should parse")
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

#[test]
fn reflink_copy_file_preserves_content_or_reports_unsupported_filesystem() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-reflink-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source");
    let destination = dir.join("destination");
    fs::write(&source, b"reflink benchmark fixture").unwrap();

    let cloned = reflink_copy_file(&source, &destination).unwrap();
    if !cloned {
        fs::copy(&source, &destination).unwrap();
    }
    assert_eq!(
        fs::read(&destination).unwrap(),
        b"reflink benchmark fixture"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn copy_file_range_file_preserves_content_or_reports_unsupported_filesystem() {
    let dir =
        std::env::temp_dir().join(format!("enclave-fsutil-copy-range-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source");
    let destination = dir.join("destination");
    fs::write(&source, b"copy file range fixture").unwrap();

    let copied = copy_file_range_file(&source, &destination).unwrap();
    if !copied {
        fs::copy(&source, &destination).unwrap();
    }
    assert_eq!(fs::read(&destination).unwrap(), b"copy file range fixture");
    let _ = fs::remove_dir_all(dir);
}

fn marker_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "enclave-creation-marker-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create marker dir");
    dir
}

#[test]
fn a_directory_without_a_marker_is_not_being_created() {
    let dir = marker_dir("absent");
    assert!(!creation_in_progress(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn this_process_marks_a_directory_it_is_creating() {
    // The marker names the creating process, so a live process protects the
    // directory and repair leaves it alone while the create runs.
    let dir = marker_dir("live");
    write_creation_marker(&dir).expect("write the marker");
    assert!(creation_in_progress(&dir));
    remove_creation_marker(&dir);
    assert!(!creation_in_progress(&dir));
    // Removing an already-removed marker is not an error.
    remove_creation_marker(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_claimed_directory_is_visible_with_its_marker_already_in_place() {
    // The reason the directory is built under the staging tree and renamed into
    // place is that the final name must never exist without a claim: repair runs
    // on every create and would otherwise read it as a leftover. The staging
    // entry has to be renamed away, not copied.
    let state_dir =
        std::env::temp_dir().join(format!("enclave-claimed-directory-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state_dir);
    let target = state_dir.join("sandboxes").join("sandbox-abc123");
    create_claimed_directory(&state_dir, "sandbox", &target).expect("claim the directory");
    assert!(target.is_dir());
    assert!(
        creation_in_progress(&target),
        "the renamed directory carries the marker that claims it"
    );
    assert!(
        !creation_staging_root(&state_dir)
            .join("sandbox")
            .join("sandbox-abc123")
            .exists(),
        "the staging entry is renamed into place, not copied"
    );
    let _ = std::fs::remove_dir_all(&state_dir);
}

#[test]
fn a_stale_staging_entry_does_not_block_the_name_it_holds() {
    // A create that died before the rename leaves its staging directory behind.
    // Its marker names a process that is gone, so the next create of that name
    // clears it instead of failing.
    let state_dir =
        std::env::temp_dir().join(format!("enclave-stale-staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state_dir);
    let target = state_dir.join("sandboxes").join("sandbox-abc123");
    let staging = creation_staging_root(&state_dir)
        .join("sandbox")
        .join("sandbox-abc123");
    std::fs::create_dir_all(&staging).expect("create the stale staging directory");
    std::fs::write(
        staging.join(CREATION_MARKER_NAME),
        format!("pid={}\nstarttime=1\n", u32::MAX),
    )
    .expect("write a stale marker");

    create_claimed_directory(&state_dir, "sandbox", &target).expect("claim over the stale entry");
    assert!(creation_in_progress(&target));
    let _ = std::fs::remove_dir_all(&state_dir);
}

#[test]
fn a_marker_from_a_dead_process_is_stale() {
    // A create that died leaves its marker behind. The recorded start time no
    // longer matches anything, so the directory is treated as an orphan again
    // rather than being protected forever.
    let dir = marker_dir("stale");
    std::fs::write(
        dir.join(CREATION_MARKER_NAME),
        format!("pid={}\nstarttime=1\n", u32::MAX),
    )
    .expect("write a stale marker");
    assert!(!creation_in_progress(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_malformed_marker_cannot_protect_a_directory() {
    let dir = marker_dir("malformed");
    for content in [
        "",
        "pid=",
        "pid=1",
        "starttime=1",
        "pid=abc\nstarttime=def\n",
    ] {
        std::fs::write(dir.join(CREATION_MARKER_NAME), content).expect("write a marker");
        assert!(
            !creation_in_progress(&dir),
            "a marker it cannot parse must not claim a live owner: {content:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
