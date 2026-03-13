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
