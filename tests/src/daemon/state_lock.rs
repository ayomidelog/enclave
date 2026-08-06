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
