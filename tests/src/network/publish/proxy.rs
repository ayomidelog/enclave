//! Proxying a published port to the workspace service.

use super::*;

#[test]
fn an_idle_connection_costs_no_cpu() {
    let _serial = serialize_heavy_connection_test();
    // A connection with nothing to carry must not wake its thread. Reaping the
    // idle timeout by polling with a short read timeout cost about 0.6 ms of CPU
    // per connection per second, which is most of a core at the connection cap.
    let service = TcpListener::bind("127.0.0.1:0").expect("bind the workspace service");
    let service_port = service.local_addr().expect("service addr").port();
    let acceptor = thread::spawn(move || {
        let mut held = Vec::new();
        for _ in 0..50 {
            let (stream, _) = service.accept().expect("accept");
            held.push(stream);
        }
        held
    });

    let publisher = PortPublisher::new();
    let host_port = free_host_port();
    publish_loopback_port(&publisher, host_port, service_port);

    let mut clients = Vec::new();
    for _ in 0..50 {
        clients.push(TcpStream::connect(("127.0.0.1", host_port)).expect("connect"));
    }
    let _held = acceptor.join().expect("join the acceptor");

    // Let the connections settle, then measure one second of doing nothing.
    thread::sleep(Duration::from_millis(500));
    let (matched, before) = connection_thread_switches();
    assert_eq!(matched, 100, "expected two copy threads per connection");
    thread::sleep(Duration::from_secs(1));
    let (_, after) = connection_thread_switches();
    let wakeups = after.saturating_sub(before);
    assert!(
        wakeups < 50,
        "50 idle connections woke their threads {wakeups} times in one second"
    );

    drop(clients);
    publisher.clear_workspace_ports("sb-publish", "ws-publish");
}

#[test]
fn withdrawing_a_port_ends_an_idle_connection() {
    // The connection thread blocks in `read`, so a withdrawal only reaches it if
    // the publisher shuts its socket down. Without that the client would stay
    // connected to a port that is no longer published.
    let service = TcpListener::bind("127.0.0.1:0").expect("bind the workspace service");
    let service_port = service.local_addr().expect("service addr").port();
    let acceptor = thread::spawn(move || {
        let (stream, _) = service.accept().expect("accept the proxied connection");
        stream
    });

    let publisher = PortPublisher::new();
    let host_port = free_host_port();
    publish_loopback_port(&publisher, host_port, service_port);

    let mut client = TcpStream::connect(("127.0.0.1", host_port)).expect("connect to the port");
    // Bound the read so a regression fails the test instead of hanging it.
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set the client read timeout");
    let _held = acceptor.join().expect("join the acceptor");

    publisher.clear_workspace_ports("sb-publish", "ws-publish");

    let mut reply = Vec::new();
    client
        .read_to_end(&mut reply)
        .expect("the withdrawn connection should close cleanly");
    assert!(reply.is_empty(), "unexpected payload: {reply:?}");
}

#[test]
fn a_published_port_proxies_a_conversation_to_the_workspace_service() {
    let service = TcpListener::bind("127.0.0.1:0").expect("bind the workspace service");
    let service_port = service.local_addr().expect("service addr").port();
    let echo = thread::spawn(move || {
        let (mut stream, _) = service.accept().expect("accept the proxied connection");
        let mut request = [0u8; 32];
        let read = stream.read(&mut request).expect("read the request");
        stream.write_all(&request[..read]).expect("write the echo");
        stream.write_all(b"-done").expect("write the marker");
        // Dropping the stream closes it, which is what ends the client's read.
    });

    let publisher = PortPublisher::new();
    let host_port = free_host_port();
    publish_loopback_port(&publisher, host_port, service_port);

    let mut client = TcpStream::connect(("127.0.0.1", host_port)).expect("connect to the port");
    client.write_all(b"hello").expect("write the request");
    let mut reply = Vec::new();
    client.read_to_end(&mut reply).expect("read the reply");
    assert_eq!(reply, b"hello-done");

    publisher.clear_workspace_ports("sb-publish", "ws-publish");
    assert!(!publisher.has_active_workspace_ports("sb-publish", "ws-publish"));
    echo.join().expect("join the echo server");
}

#[test]
fn clearing_published_ports_releases_the_host_port() {
    // A stop proves the listeners were released by rebinding the port it held.
    // That only holds if shutdown waits for the listener to close, not merely for
    // the shutdown flag to be set.
    let service = TcpListener::bind("127.0.0.1:0").expect("bind the workspace service");
    let service_port = service.local_addr().expect("service addr").port();

    let publisher = PortPublisher::new();
    let host_port = free_host_port();
    publish_loopback_port(&publisher, host_port, service_port);
    assert!(publisher.has_active_workspace_ports("sb-publish", "ws-publish"));

    publisher.clear_workspace_ports("sb-publish", "ws-publish");
    assert!(!publisher.has_active_workspace_ports("sb-publish", "ws-publish"));

    let rebind = TcpListener::bind(("127.0.0.1", host_port));
    assert!(
        rebind.is_ok(),
        "the published port is still held: {rebind:?}"
    );
}

/// One workspace can hold several published ports, and one publisher can serve
/// several workspaces at once.
///
/// The ports a workspace publishes are replaced as a set on every reconcile, so
/// the failure worth guarding against is a reconcile that drops or takes over a
/// binding that belongs to another workspace: the maps are keyed by workspace, and
/// two workspaces publishing the same host port would be a silent conflict.
#[test]
fn several_published_ports_coexist_across_workspaces() {
    let first_service = TcpListener::bind("127.0.0.1:0").expect("bind the first service");
    let first_service_port = first_service.local_addr().expect("service addr").port();
    let second_service = TcpListener::bind("127.0.0.1:0").expect("bind the second service");
    let second_service_port = second_service.local_addr().expect("service addr").port();

    let publisher = PortPublisher::new();
    let [first_host_port, second_host_port, third_host_port, other_host_port] =
        free_host_ports(4)[..]
    else {
        unreachable!("asked for four ports")
    };
    publish_loopback_port(&publisher, first_host_port, first_service_port);

    // A second port on the same workspace, and a port on a second workspace.
    let specs = [
        PublishedPortSpec {
            host_ip: "127.0.0.1".to_string(),
            host_port: second_host_port,
            workspace_port: first_service_port,
            protocol: "tcp".to_string(),
        },
        PublishedPortSpec {
            host_ip: "127.0.0.1".to_string(),
            host_port: third_host_port,
            workspace_port: first_service_port,
            protocol: "tcp".to_string(),
        },
    ];
    let statuses = publisher
        .apply_workspace_ports_strict(
            "sb-publish",
            "ws-publish",
            std::process::id(),
            "127.0.0.1",
            &specs,
        )
        .expect("publish both ports of the first workspace");
    assert_eq!(statuses.len(), 2);

    let other = PublishedPortSpec {
        host_ip: "127.0.0.1".to_string(),
        host_port: other_host_port,
        workspace_port: second_service_port,
        protocol: "tcp".to_string(),
    };
    let other_statuses = publisher
        .apply_workspace_ports_strict(
            "sb-publish",
            "ws-other",
            std::process::id(),
            "127.0.0.1",
            std::slice::from_ref(&other),
        )
        .expect("publish the second workspace's port");
    assert_eq!(other_statuses.len(), 1);

    // Every binding answers on its own host port, which is what proves they were
    // not collapsed into one another.
    for host_port in [second_host_port, third_host_port, other.host_port] {
        TcpStream::connect(("127.0.0.1", host_port))
            .unwrap_or_else(|error| panic!("connect to {host_port}: {error}"));
    }

    // Releasing one workspace leaves the other's ports held and the released host
    // ports free to bind.
    publisher.clear_workspace_ports("sb-publish", "ws-publish");
    assert!(publisher.has_active_workspace_ports("sb-publish", "ws-other"));
    for host_port in [first_host_port, second_host_port, third_host_port] {
        let rebind = TcpListener::bind(("127.0.0.1", host_port));
        assert!(rebind.is_ok(), "port {host_port} is still held: {rebind:?}");
    }
    TcpStream::connect(("127.0.0.1", other.host_port))
        .expect("the other workspace's port must still answer");

    publisher.clear_workspace_ports("sb-publish", "ws-other");
}

#[test]
fn a_published_port_rejects_a_connection_it_cannot_proxy() {
    // Nothing is listening on the workspace port, so the proxy must close the
    // client rather than hold it. The client sees end of file.
    let unavailable = free_host_port();

    let publisher = PortPublisher::new();
    let host_port = free_host_port();
    publish_loopback_port(&publisher, host_port, unavailable);

    let mut client = TcpStream::connect(("127.0.0.1", host_port)).expect("connect to the port");
    let mut reply = Vec::new();
    client
        .read_to_end(&mut reply)
        .expect("read the closed connection");
    assert!(reply.is_empty(), "unexpected payload: {reply:?}");

    publisher.clear_workspace_ports("sb-publish", "ws-publish");
}
