//! Unmounting workspace storage, and deciding which mounts are Enclave owns.
//!
//! The rule these pin is that a mount is only released when Enclave can attribute it
//! to itself, and that a failed unmount reports the errno rather than a summary.

use super::*;

#[test]
fn mountinfo_parser_preserves_nested_mounts_for_reverse_cleanup() {
    let mountinfo = concat!(
        "100 1 0:50 / /tmp/enclave/ws/fs rw,relatime - ext4 /dev/loop0 rw\n",
        "101 100 0:51 / /tmp/enclave/ws/fs/cache\\040data rw,relatime - tmpfs tmpfs rw\n"
    );
    let mut mountpoints = parse_mountinfo_mountpoints(mountinfo)
        .into_iter()
        .filter(|path| path.starts_with("/tmp/enclave/ws/fs"))
        .collect::<Vec<_>>();
    mountpoints.sort_by_key(|path| std::cmp::Reverse(path.components().count()));

    assert_eq!(
        mountpoints,
        vec![
            PathBuf::from("/tmp/enclave/ws/fs/cache data"),
            PathBuf::from("/tmp/enclave/ws/fs"),
        ]
    );
}

#[test]
fn dead_workspace_owner_allows_lazy_unmount_fallback() {
    let mut workspace = workspace_fixture();
    workspace.runtime_pid = Some(u32::MAX);
    workspace.runtime_starttime_ticks = Some(1);
    assert!(workspace_owner_is_dead(&workspace));
}

#[test]
fn missing_starttime_does_not_block_lazy_unmount_fallback() {
    let mut workspace = workspace_fixture();
    workspace.runtime_pid = Some(std::process::id());
    workspace.runtime_starttime_ticks = None;
    assert!(workspace_owner_is_dead(&workspace));
}

#[test]
fn loop_device_parser_extracts_only_loop_backings() {
    let output = "/dev/loop7: []: (/tmp/a/fs.img)\n/dev/loop-control: []: (/tmp/control)\n";
    assert_eq!(parse_losetup_for_image(output), vec!["/dev/loop7"]);
}

#[test]
fn unmount_error_reports_errno_in_errno_field() {
    let error = std::io::Error::from_raw_os_error(libc::EBUSY);
    let formatted = unmount_error(Path::new("/tmp/enclave/ws/fs"), &error).to_string();
    assert!(formatted.contains("target=/tmp/enclave/ws/fs"));
    assert!(formatted.contains("errno=EBUSY(16)"));
    assert!(!formatted.contains("errno=/tmp/enclave/ws/fs"));
}

#[test]
fn a_mount_below_a_workspace_that_enclave_did_not_create_is_reported_not_owned() {
    let root = "/srv/enclave/sandboxes/sb/workspaces/ws";
    let raw = format!(
        "100 1 8:1 {root}/fs {root}/fs rw - ext4 /dev/sda1 rw\n\
         200 1 0:60 / {root}/operator-backup rw,nosuid - tmpfs tmpfs rw\n"
    );
    let snapshot = crate::fsutil::MountInfoSnapshot::parse(&raw);

    let (owned, foreign) = mounts_below(&snapshot, Path::new(root));
    assert_eq!(
        owned.len(),
        1,
        "the state-backed bind is Enclave's own: {owned:?}"
    );
    assert_eq!(foreign.len(), 1, "the tmpfs is foreign: {foreign:?}");
    assert!(
        foreign[0].contains("operator-backup") && foreign[0].contains("source tmpfs"),
        "foreign mount should be named with its source: {foreign:?}"
    );
}

#[test]
fn remaining_mount_detail_separates_enclave_leftovers_from_foreign_mounts() {
    let detail = remaining_mount_detail(
        &["/srv/enclave/sandboxes/sb/workspaces/ws/fs (source /dev/loop3)".to_string()],
        &["/srv/enclave/sandboxes/sb/workspaces/ws/backup (source tmpfs)".to_string()],
    );
    assert!(
        detail.contains("1 Enclave mount(s) survived unmount"),
        "{detail}"
    );
    assert!(
        detail.contains("were not created by Enclave and were left in place"),
        "{detail}"
    );
}

/// A busy mount names the processes that hold it, and their mount namespaces.
///
/// An EBUSY unmount reports only an errno, and the errno does not say who is in
/// the way. The holder list is what turns it into an actionable diagnosis: the pid
/// is the process to look at and the namespace inode is what proves the pid is the
/// one that holds the mount rather than a reused one. The test mounts a filesystem
/// itself and asks the same function the unmount error uses, so what it proves is
/// that the function reads the kernel record rather than that a particular message
/// was formatted.
///
/// The list is capped, so the test does not assume its own pid is in it. What it
/// checks instead is that every entry pairs a pid with the namespace that pid is
/// actually in, which is the property the diagnosis rests on.
#[test]
#[ignore = "requires root privileges and mount support"]
fn a_busy_mount_names_its_holder_pid_and_namespace() {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let dir = std::env::temp_dir().join(format!("enclave-unmount-holders-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the mount point");

    let mount_point = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).unwrap();
    let source = std::ffi::CString::new("tmpfs").unwrap();
    let fstype = std::ffi::CString::new("tmpfs").unwrap();
    let mounted = unsafe {
        libc::mount(
            source.as_ptr(),
            mount_point.as_ptr(),
            fstype.as_ptr(),
            0,
            std::ptr::null(),
        )
    };
    assert_eq!(mounted, 0, "failed to mount tmpfs at {}", dir.display());

    let holders = mount_holders(&dir);
    assert!(
        !holders.is_empty(),
        "a mounted path must name the processes whose mount table lists it"
    );
    for holder in &holders {
        let (pid, namespace) = holder
            .strip_prefix("pid=")
            .and_then(|rest| rest.split_once("@"))
            .unwrap_or_else(|| panic!("holder entry is not pid=<n>@<ns>: {holder}"));
        let pid: u32 = pid.parse().expect("the holder entry names a pid");
        let actual = std::fs::read_link(format!("/proc/{pid}/ns/mnt"))
            .expect("read the holder mount namespace");
        assert_eq!(
            actual.to_string_lossy(),
            namespace,
            "the namespace in the entry is not the one pid {pid} is in"
        );
    }

    // A path that is not a mount is nobody, so the same function answers with an
    // empty list rather than naming every process that shares a directory name.
    let plain = dir.join("not-a-mount");
    std::fs::create_dir_all(&plain).expect("create a plain directory");
    assert!(mount_holders(&plain).is_empty());

    let unmounted = unsafe { libc::umount(mount_point.as_ptr()) };
    assert_eq!(unmounted, 0, "failed to unmount {}", dir.display());
    assert!(
        mount_holders(&dir).is_empty(),
        "an unmounted path must name no holder"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
