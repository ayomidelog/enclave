use super::prepare_runtime_paths;
use std::fs;
use std::os::unix::net::UnixListener;

#[test]
fn prepare_runtime_paths_removes_stale_socket_file() {
    let dir = std::env::temp_dir().join(format!("enclave-daemon-stale-{}", std::process::id()));
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("cleanup stale test dir");
    }
    fs::create_dir_all(&dir).expect("create test dir");
    let socket = dir.join("daemon.sock");
    let pid = dir.join("daemon.pid");

    let listener = UnixListener::bind(&socket).expect("bind test socket");
    drop(listener);
    assert!(
        socket.exists(),
        "socket path should still exist as stale file"
    );

    prepare_runtime_paths(&socket, &pid).expect("stale socket should be cleaned");
    assert!(
        !socket.exists(),
        "stale socket file should be removed before daemon bind"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn prepare_runtime_paths_rejects_active_socket() {
    let dir = std::env::temp_dir().join(format!("enclave-daemon-active-{}", std::process::id()));
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("cleanup active test dir");
    }
    fs::create_dir_all(&dir).expect("create test dir");
    let socket = dir.join("daemon.sock");
    let pid = dir.join("daemon.pid");

    let listener = UnixListener::bind(&socket).expect("bind active socket");
    let err = prepare_runtime_paths(&socket, &pid).expect_err("active socket should fail");
    assert!(
        err.to_string().contains("is active"),
        "unexpected error: {err:#}"
    );
    drop(listener);
    let _ = fs::remove_file(&socket);
    let _ = fs::remove_dir_all(&dir);
}
