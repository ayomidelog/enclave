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
