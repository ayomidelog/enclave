use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;

use super::*;

#[test]
fn a_missing_socket_is_reported_as_unreachable() {
    let path = std::env::temp_dir().join(format!("enclave-missing-{}", uuid::Uuid::new_v4()));
    let err = send_request(&path, "ping", json!({})).expect_err("missing socket");

    assert!(is_daemon_unreachable(&err), "unexpected error: {err:#}");
    assert!(format!("{err:#}").contains("daemon socket not found"));
}

#[test]
fn a_daemon_that_answers_with_an_error_is_reachable() {
    let dir = std::env::temp_dir().join(format!("enclave-client-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("create socket dir");
    let path = dir.join("manager.sock");
    let listener = UnixListener::bind(&path).expect("bind socket");
    // The client refuses a group/world writable socket, and the test process
    // may run with a permissive umask, so set the mode the daemon would.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("restrict socket");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request");
        // Read the whole request line before answering. The client writes the
        // payload and its terminator separately, so replying after one read can
        // close the connection while the terminator is still in flight.
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.contains(&b'\n') {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => request.extend_from_slice(&buffer[..read]),
            }
        }
        let response = json!({
            "ok": false,
            "result": null,
            "error": "policy denied action 'ping' for uid 0",
        });
        stream
            .write_all(response.to_string().as_bytes())
            .expect("write response");
        stream.write_all(b"\n").expect("write terminator");
    });

    let err = send_request(&path, "ping", json!({})).expect_err("policy denial");
    assert!(!is_daemon_unreachable(&err), "unexpected error: {err:#}");
    assert!(
        format!("{err:#}").contains("policy denied"),
        "unexpected error: {err:#}"
    );

    server.join().expect("server thread");
    let _ = fs::remove_dir_all(dir);
}
