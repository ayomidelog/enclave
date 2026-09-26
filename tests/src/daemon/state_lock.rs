use super::*;

#[test]
fn state_lock_excludes_second_daemon_and_reports_owner() {
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-state-lock-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&state_dir);
    let socket = state_dir.join("manager.sock");

    let first = acquire_state_lock(&state_dir, &socket).expect("acquire first lock");
    let record = read_state_lock_record(&state_dir)
        .expect("read lock record")
        .expect("lock record exists");
    assert_eq!(record.pid, std::process::id());
    assert_eq!(record.socket, socket.to_string_lossy());
    assert_eq!(record.binary_version, env!("CARGO_PKG_VERSION"));
    assert!(!record.started_at.is_empty());

    let error = acquire_state_lock(&state_dir, &socket).expect_err("second lock must fail");
    let message = format!("{error:#}");
    assert!(message.contains("already owned"));
    assert!(message.contains(&format!("pid={}", std::process::id())));

    drop(first);
    assert!(!state_lock_path(&state_dir).exists());
    let _ = std::fs::remove_dir_all(&state_dir);
}

#[test]
fn dropping_a_lock_does_not_delete_a_replacement_file() {
    // A daemon that exits removes the lock file, and one that is starting at the
    // same moment can have already replaced it. Deleting the replacement would
    // remove that daemon's exclusion and let a third daemon start against the same
    // state directory, so the removal only happens while the file at the path is
    // still the one this lock holds.
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-state-lock-replace-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&state_dir);
    let socket = state_dir.join("manager.sock");

    let held = acquire_state_lock(&state_dir, &socket).expect("acquire first lock");
    let path = state_lock_path(&state_dir);

    // Replace the lock file the way a competing daemon would after the first one
    // unlinked it: a fresh file with a fresh record.
    std::fs::remove_file(&path).expect("remove the original lock file");
    std::fs::write(&path, b"{\"replacement\":true}\n").expect("write the replacement");

    drop(held);

    assert!(
        path.exists(),
        "the lock this daemon held is gone, so dropping it must not delete the file that replaced it"
    );
    assert_eq!(
        std::fs::read(&path).expect("read the replacement"),
        b"{\"replacement\":true}\n"
    );
    let _ = std::fs::remove_dir_all(&state_dir);
}

#[test]
fn a_lock_path_that_is_not_a_regular_file_is_refused() {
    // Locking a symlink or a fifo would not exclude another daemon from this state
    // directory, so the path is refused rather than replaced.
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-state-lock-notafile-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&state_dir);
    std::fs::create_dir_all(&state_dir).expect("create state dir");
    let socket = state_dir.join("manager.sock");
    std::fs::create_dir(state_lock_path(&state_dir)).expect("make the lock path a directory");

    let error = acquire_state_lock(&state_dir, &socket).expect_err("a directory must be refused");
    assert!(
        format!("{error:#}").contains("not a regular file"),
        "unexpected error: {error:#}"
    );
    let _ = std::fs::remove_dir_all(&state_dir);
}

#[test]
fn acquiring_writes_its_record_into_the_file_at_the_lock_path() {
    // The lock is on the inode and the record is what `doctor` reads by path, so
    // the two have to be the same file. This is the invariant the retry loop
    // restores when a competing daemon replaces the file mid-acquire.
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-state-lock-inode-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&state_dir);
    let socket = state_dir.join("manager.sock");

    let held = acquire_state_lock(&state_dir, &socket).expect("acquire the lock");
    let at_path = std::fs::metadata(state_lock_path(&state_dir)).expect("stat the lock file");
    let locked = std::fs::metadata(format!("/proc/self/fd/{}", held.file.as_raw_fd()))
        .expect("stat the locked descriptor");
    assert_eq!(at_path.dev(), locked.dev());
    assert_eq!(at_path.ino(), locked.ino());
    drop(held);
    let _ = std::fs::remove_dir_all(&state_dir);
}
