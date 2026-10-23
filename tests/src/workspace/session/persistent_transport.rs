use super::*;
use anyhow::Context;

#[test]
fn a_client_disconnect_is_distinct_from_a_helper_timeout() {
    let disconnected = anyhow::Error::new(ClientDisconnected {
        message: "client left".to_string(),
    });
    assert!(ClientDisconnected::is_in(&disconnected));
    assert!(!HelperTimeout::is_in(&disconnected));

    let timed_out = anyhow::Error::new(HelperTimeout {
        message: "helper stopped answering".to_string(),
    });
    assert!(HelperTimeout::is_in(&timed_out));
    assert!(!ClientDisconnected::is_in(&timed_out));
}

#[test]
fn a_wrapped_helper_failure_keeps_its_identity() {
    let error: anyhow::Result<()> = Err(anyhow::Error::new(HelperClosed {
        message: "helper closed".to_string(),
    }));
    let error = error
        .context("failed to run a command in workspace dev")
        .unwrap_err();
    assert!(HelperClosed::is_in(&error));
    assert!(!HelperTimeout::is_in(&error));
}

#[test]
fn a_closed_client_socket_is_reported_as_a_disconnect() {
    let (stream, peer) = UnixStream::pair().expect("socket pair");
    assert!(!client_disconnected(&stream));
    drop(peer);
    let deadline = Instant::now() + Duration::from_secs(1);
    while !client_disconnected(&stream) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(client_disconnected(&stream));
}
