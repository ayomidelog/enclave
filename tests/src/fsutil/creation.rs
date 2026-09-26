//! The marker that says a directory is being created right now.
//!
//! A live create owns its directory until it commits, so the marker is what stops a
//! repair from adopting it. A marker left by a process that is gone protects nothing.

use super::*;

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
