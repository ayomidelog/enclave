use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;

use super::*;

/// Answer one request on a fresh socket with `response`, returning the socket
/// directory, the socket path, and the server thread.
///
/// The client refuses a group/world writable socket and the test process may run
/// with a permissive umask, so the mode the daemon would set is set here too.
fn serve_one_response(
    response: Value,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    std::thread::JoinHandle<()>,
) {
    let dir = std::env::temp_dir().join(format!("enclave-client-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("create socket dir");
    let path = dir.join("manager.sock");
    let listener = UnixListener::bind(&path).expect("bind socket");
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
        stream
            .write_all(response.to_string().as_bytes())
            .expect("write response");
        stream.write_all(b"\n").expect("write terminator");
    });
    (dir, path, server)
}

#[test]
fn a_missing_socket_is_reported_as_unreachable() {
    let path = std::env::temp_dir().join(format!("enclave-missing-{}", uuid::Uuid::new_v4()));
    let err = send_request(&path, "ping", json!({})).expect_err("missing socket");

    assert!(is_daemon_unreachable(&err), "unexpected error: {err:#}");
    assert!(format!("{err:#}").contains("daemon socket not found"));
}

#[test]
fn a_daemon_that_answers_with_an_error_is_reachable() {
    let (dir, path, server) = serve_one_response(json!({
        "ok": false,
        "result": null,
        "error": "policy denied action 'ping' for uid 0",
    }));

    let err = send_request(&path, "ping", json!({})).expect_err("policy denial");
    assert!(!is_daemon_unreachable(&err), "unexpected error: {err:#}");
    assert!(
        format!("{err:#}").contains("policy denied"),
        "unexpected error: {err:#}"
    );

    server.join().expect("server thread");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_response_larger_than_the_old_ceiling_is_accepted() {
    // 600_000 bytes is the size the report used to reproduce the failure. It is
    // above the 512 KiB ceiling the client had and far below what a `workspace
    // exec` result may carry, so refusing it rejected a response the daemon was
    // entitled to send after the command had already run.
    let payload = "x".repeat(600_000);
    let (dir, path, server) = serve_one_response(json!({
        "ok": true,
        "result": {"stdout": payload, "exit_code": 0},
        "operation_id": "op-large",
    }));

    let value = send_request(&path, "workspace.exec", json!({})).expect("large response");

    assert_eq!(value["stdout"].as_str().expect("stdout").len(), 600_000);
    server.join().expect("server thread");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn the_read_ceiling_covers_the_worst_response_the_daemon_can_send() {
    // A `workspace exec` returns stdout and stderr, each captured up to
    // `MAX_HELPER_OUTPUT_BYTES`, and JSON escaping can expand a captured byte to
    // six. A ceiling below that product refuses a reply the daemon was entitled
    // to send, after the command has already run.
    let worst_case = 2 * MAX_HELPER_OUTPUT_BYTES * JSON_ESCAPE_WORST_CASE;
    assert!(
        MAX_RESPONSE_BYTES >= worst_case,
        "the read ceiling ({MAX_RESPONSE_BYTES}) must cover the worst-case response ({worst_case})"
    );
}
