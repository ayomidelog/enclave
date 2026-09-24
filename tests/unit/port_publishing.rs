use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use enclave::workspace::{PortPublisher, PublishedPortSpec, PublishedPortState};

#[test]
fn strict_publish_proxies_tcp_connections() {
    let target_listener = TcpListener::bind("127.0.0.1:0").expect("bind target");
    let target_port = target_listener.local_addr().expect("target addr").port();
    let host_port = reserve_port();

    let server = thread::spawn(move || {
        let (mut stream, _) = target_listener.accept().expect("accept target");
        let mut payload = [0_u8; 4];
        stream.read_exact(&mut payload).expect("read payload");
        assert_eq!(&payload, b"ping");
        stream.write_all(b"pong").expect("write response");
    });

    let publisher = PortPublisher::new();
    let spec =
        PublishedPortSpec::parse(&format!("127.0.0.1:{host_port}:{target_port}/tcp")).unwrap();
    let statuses = publisher
        .apply_workspace_ports_strict("sb", "ws", std::process::id(), "127.0.0.1", &[spec])
        .expect("publish port");
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].state, PublishedPortState::Active);

    let mut client = TcpStream::connect(("127.0.0.1", host_port)).expect("connect host port");
    client.write_all(b"ping").expect("write host payload");
    let mut response = [0_u8; 4];
    client
        .read_exact(&mut response)
        .expect("read host response");
    assert_eq!(&response, b"pong");

    publisher.clear_workspace_ports("sb", "ws");
    server.join().expect("server thread");
}

#[test]
fn strict_publish_reports_host_port_conflicts() {
    let occupied = TcpListener::bind("127.0.0.1:0").expect("occupy host port");
    let host_port = occupied.local_addr().expect("occupied addr").port();
    let target_port = reserve_port();

    let publisher = PortPublisher::new();
    let spec =
        PublishedPortSpec::parse(&format!("127.0.0.1:{host_port}:{target_port}/tcp")).unwrap();
    let err = publisher
        .apply_workspace_ports_strict("sb", "ws", std::process::id(), "127.0.0.1", &[spec])
        .expect_err("host port conflict");
    assert!(err.to_string().contains("host port already in use"));
}

#[test]
fn clear_workspace_ports_releases_bound_listener() {
    let target_port = reserve_port();
    let host_port = reserve_port();
    let publisher = PortPublisher::new();
    let spec =
        PublishedPortSpec::parse(&format!("127.0.0.1:{host_port}:{target_port}/tcp")).unwrap();

    publisher
        .apply_workspace_ports_strict("sb", "ws", std::process::id(), "127.0.0.1", &[spec])
        .expect("publish host port");
    publisher.clear_workspace_ports("sb", "ws");

    wait_for(
        || TcpListener::bind(("127.0.0.1", host_port)).is_ok(),
        Duration::from_secs(2),
    );
}

#[test]
fn reconcile_workspace_ports_marks_failures_without_returning_error() {
    let occupied = TcpListener::bind("127.0.0.1:0").expect("occupy host port");
    let host_port = occupied.local_addr().expect("occupied addr").port();
    let target_port = reserve_port();

    let publisher = PortPublisher::new();
    let spec =
        PublishedPortSpec::parse(&format!("127.0.0.1:{host_port}:{target_port}/tcp")).unwrap();
    let statuses = publisher
        .reconcile_workspace_ports("sb", "ws", std::process::id(), "127.0.0.1", &[spec])
        .expect("best-effort reconcile");

    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].state, PublishedPortState::Failed);
    assert!(statuses[0]
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("host port already in use"));
}

#[test]
fn strict_publish_proxies_multiple_http_like_requests() {
    let target_listener = TcpListener::bind("127.0.0.1:0").expect("bind target");
    let target_port = target_listener.local_addr().expect("target addr").port();
    let host_port = reserve_port();

    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = target_listener.accept().expect("accept target");
            let mut request = [0_u8; 256];
            let read = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.contains("GET / HTTP/1.1"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .expect("write response");
        }
    });

    let publisher = PortPublisher::new();
    let spec = PublishedPortSpec::parse(&format!("127.0.0.1:{host_port}:{target_port}/tcp"))
        .expect("parse published port spec");
    let statuses = publisher
        .apply_workspace_ports_strict("sb", "ws", std::process::id(), "127.0.0.1", &[spec])
        .expect("publish host port");
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].state, PublishedPortState::Active);

    for _ in 0..2 {
        let mut client = TcpStream::connect(("127.0.0.1", host_port)).expect("connect host port");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("write request");
        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("read full response");
        assert!(response.contains("HTTP/1.1 200 OK"));
        assert!(response.ends_with("ok"));
    }

    publisher.clear_workspace_ports("sb", "ws");
    server.join().expect("server thread");
}

/// Accepts connections and echoes bytes back, holding each connection open until
/// its peer closes.
fn spawn_echo_target() -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind echo target");
    let port = listener.local_addr().expect("echo target addr").port();
    let handle = thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                return;
            };
            thread::spawn(move || {
                let mut buffer = [0_u8; 1024];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => return,
                        Ok(read) => {
                            if stream.write_all(&buffer[..read]).is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
    });
    (port, handle)
}

/// A port whose own connection budget is exhausted must not consume the budget
/// every other published port draws from.
///
/// Before the two-tier budget, all published ports shared one pool, so a single
/// port with idle connections could refuse connections to every other port.
#[test]
fn a_saturated_published_port_does_not_starve_another() {
    let (target_a_port, _target_a) = spawn_echo_target();
    let (target_b_port, _target_b) = spawn_echo_target();
    let host_a = reserve_port();
    let host_b = reserve_port();

    let publisher = PortPublisher::new();
    let spec_a =
        PublishedPortSpec::parse(&format!("127.0.0.1:{host_a}:{target_a_port}/tcp")).unwrap();
    let spec_b =
        PublishedPortSpec::parse(&format!("127.0.0.1:{host_b}:{target_b_port}/tcp")).unwrap();
    publisher
        .apply_workspace_ports_strict("sb", "ws-a", std::process::id(), "127.0.0.1", &[spec_a])
        .expect("publish port a");
    publisher
        .apply_workspace_ports_strict("sb", "ws-b", std::process::id(), "127.0.0.1", &[spec_b])
        .expect("publish port b");

    // Hold connections to port A until the publisher refuses one, which is the
    // signal that port A has used up its own budget. A held connection stays
    // silent, so a short read timeout distinguishes it from a refused one.
    let mut held = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut saturated = false;
    while Instant::now() < deadline {
        let mut client = TcpStream::connect(("127.0.0.1", host_a)).expect("connect port a");
        client
            .set_read_timeout(Some(Duration::from_millis(20)))
            .expect("set read timeout");
        let mut byte = [0_u8; 1];
        match client.read(&mut byte) {
            Ok(0) => {
                saturated = true;
                break;
            }
            Ok(_) => panic!("port A unexpectedly returned data"),
            Err(_) => held.push(client),
        }
    }
    assert!(saturated, "port A never reached its connection limit");

    // Port B must still complete a round trip while port A is saturated.
    let mut client = TcpStream::connect(("127.0.0.1", host_b)).expect("connect port b");
    client.write_all(b"ping").expect("write port b payload");
    let mut response = [0_u8; 4];
    client
        .read_exact(&mut response)
        .expect("read port b response");
    assert_eq!(&response, b"ping");

    publisher.clear_workspace_ports("sb", "ws-a");
    publisher.clear_workspace_ports("sb", "ws-b");
}

fn reserve_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve port");
    let port = listener.local_addr().expect("reserved addr").port();
    drop(listener);
    port
}

fn wait_for<F>(mut predicate: F, timeout: Duration)
where
    F: FnMut() -> bool,
{
    let start = Instant::now();
    while start.elapsed() < timeout {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("condition was not met within {:?}", timeout);
}
