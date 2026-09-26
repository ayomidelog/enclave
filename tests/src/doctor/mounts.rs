//! Telling a mount Enclave left behind apart from one it never owned.
//!
//! This is the rule the cleanup safety rests on, so the tests that a foreign mount
//! is never named as Enclave are here rather than beside the parsing they share a
//! fixture with.

use super::*;

#[test]
fn classify_sandbox_mounts_separates_enclave_orphans_from_foreign_mounts() {
    let sandboxes = Path::new("/root/.local/state/enclave/sandboxes");
    let snapshot = crate::fsutil::MountInfoSnapshot::parse(concat!(
        // Enclave's overlay for a workspace whose runtime is gone: an orphan.
        "100 29 0:50 / /root/.local/state/enclave/sandboxes/sb/workspaces/gone/home-merged rw - overlay overlay rw\n",
        // Enclave's overlay for a workspace that is still running: expected.
        "101 29 0:51 / /root/.local/state/enclave/sandboxes/sb/workspaces/live/home-merged rw - overlay overlay rw\n",
        // An operator's tmpfs under a workspace path: not Enclave's to remove.
        "200 29 0:60 / /root/.local/state/enclave/sandboxes/sb/workspaces/gone/operator rw - tmpfs tmpfs rw\n"
    ));
    let active = vec![PathBuf::from(
        "/root/.local/state/enclave/sandboxes/sb/workspaces/live",
    )];

    let (orphaned, foreign) = classify_sandbox_mounts(&snapshot, sandboxes, &active);
    assert_eq!(
        orphaned.len(),
        1,
        "only the gone workspace is an orphan: {orphaned:?}"
    );
    assert!(orphaned[0].contains("gone/home-merged"), "{orphaned:?}");
    assert_eq!(foreign.len(), 1, "the tmpfs is foreign: {foreign:?}");
    assert!(
        foreign[0].contains("gone/operator") && foreign[0].contains("source tmpfs"),
        "a foreign mount is named with its source: {foreign:?}"
    );
}

#[test]
fn a_foreign_mount_is_never_reported_as_an_enclave_orphan() {
    let sandboxes = Path::new("/root/.local/state/enclave/sandboxes");
    let snapshot = crate::fsutil::MountInfoSnapshot::parse(concat!(
        "200 29 8:1 / /root/.local/state/enclave/sandboxes/sb/workspaces/ws/backup rw - ext4 /dev/sdb1 rw\n"
    ));

    let (orphaned, foreign) = classify_sandbox_mounts(&snapshot, sandboxes, &[]);
    assert!(
        orphaned.is_empty(),
        "a mount Enclave did not create is not an Enclave orphan: {orphaned:?}"
    );
    assert_eq!(foreign.len(), 1, "{foreign:?}");
}

#[test]
fn doctor_repair_report_defaults_foreign_stale_mounts_to_empty() {
    // An older daemon's report has no such field, and "no evidence" must not
    // read as "a foreign mount was found".
    let report: DoctorRepairReport = serde_json::from_str(
        r#"{"registry":{"added_sandboxes":0,"removed_sandboxes":0,"added_workspaces":0,"removed_workspaces":0},"unmounted_stale_mounts":0,"reconciled_workspace_mounts":0,"daemon_state_consistent":true}"#,
    )
    .expect("a report without the field should deserialize");
    assert!(report.foreign_stale_mounts.is_empty());
}

#[test]
fn orphaned_mount_check_counts_enclave_leftovers_and_names_foreign_mounts() {
    let state_dir = Path::new("/srv/enclave");
    let sandboxes = state_dir.join("sandboxes");
    // One Enclave overlay left behind, one Enclave overlay still in use by a
    // running workspace, and one tmpfs the operator placed under a workspace.
    let snapshot = crate::fsutil::MountInfoSnapshot::parse(concat!(
        "100 1 0:50 / /srv/enclave/sandboxes/sb/workspaces/gone/home-merged rw - overlay overlay rw\n",
        "101 1 0:51 / /srv/enclave/sandboxes/sb/workspaces/live/home-merged rw - overlay overlay rw\n",
        "102 1 0:60 / /srv/enclave/sandboxes/sb/workspaces/live/backup rw - tmpfs tmpfs rw\n"
    ));
    let active_roots = vec![PathBuf::from("/srv/enclave/sandboxes/sb/workspaces/live")];

    let (orphaned, foreign) = classify_sandbox_mounts(&snapshot, &sandboxes, &active_roots);
    assert_eq!(
        orphaned.len(),
        1,
        "only the leftover overlay is orphaned: {orphaned:?}"
    );
    assert!(orphaned[0].contains("gone/home-merged"), "{orphaned:?}");
    assert_eq!(foreign.len(), 1, "the tmpfs is foreign: {foreign:?}");
    assert!(
        foreign[0].contains("backup") && foreign[0].contains("source tmpfs"),
        "{foreign:?}"
    );
}

#[test]
fn a_foreign_mount_under_a_workspace_is_not_reported_as_an_enclave_leftover() {
    let state_dir = Path::new("/srv/enclave");
    let sandboxes = state_dir.join("sandboxes");
    let snapshot = crate::fsutil::MountInfoSnapshot::parse(concat!(
        "200 1 8:1 / /srv/enclave/sandboxes/sb/workspaces/ws/backup rw - ext4 /dev/sda1 rw\n"
    ));

    let (orphaned, foreign) = classify_sandbox_mounts(&snapshot, &sandboxes, &[]);
    assert!(
        orphaned.is_empty(),
        "a mount Enclave did not create is not an Enclave leftover: {orphaned:?}"
    );
    assert_eq!(foreign.len(), 1, "{foreign:?}");
}
