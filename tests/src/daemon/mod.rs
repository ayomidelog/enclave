use super::socket::prepare_runtime_paths;
use super::wait_for_listener;
use std::fs;
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;
use std::time::Duration;

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

#[test]
fn wait_for_listener_returns_when_a_connection_is_ready() {
    let dir = std::env::temp_dir().join(format!("enclave-daemon-poll-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create poll test dir");
    let socket = dir.join("daemon.sock");
    let listener = UnixListener::bind(&socket).expect("bind poll socket");
    listener.set_nonblocking(true).expect("set nonblocking");
    let connector = thread::spawn({
        let socket = socket.clone();
        move || {
            thread::sleep(Duration::from_millis(20));
            let _ = UnixStream::connect(socket);
        }
    });

    let shutdown_wait = super::shutdown::install_shutdown_wait().expect("create the wakeup pipe");
    wait_for_listener(&listener, &shutdown_wait).expect("poll should report readiness");
    assert!(listener.accept().is_ok());
    connector.join().expect("connector thread");
    let _ = fs::remove_dir_all(&dir);
}

/// A shutdown has to end the accept loop's wait, which blocks indefinitely.
///
/// The loop no longer wakes on a timer, so nothing but the pipe interrupts it. A
/// shutdown requested over the socket arrives on a worker thread, where no signal
/// is delivered, which is the case this covers.
#[test]
fn wait_for_listener_returns_when_a_shutdown_is_requested() {
    let dir = std::env::temp_dir().join(format!("enclave-daemon-wakeup-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create wakeup test dir");
    let socket = dir.join("daemon.sock");
    let listener = UnixListener::bind(&socket).expect("bind wakeup socket");
    let shutdown_wait = super::shutdown::install_shutdown_wait().expect("create the wakeup pipe");

    let waker = thread::spawn(|| {
        thread::sleep(Duration::from_millis(20));
        super::shutdown::wake_shutdown_wait();
    });

    let started = std::time::Instant::now();
    wait_for_listener(&listener, &shutdown_wait).expect("a wakeup should end the wait");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the wait must end on the wakeup rather than on a timer"
    );
    waker.join().expect("waker thread");
    let _ = fs::remove_dir_all(&dir);
}
