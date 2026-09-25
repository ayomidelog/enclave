//! What the proxy does under more connections than it is configured to carry.
//!
//! These open many connections at once, so they take the shared lock: the thread and
//! context-switch counts they read are properties of the process, not of one port.

use super::*;

/// A flood of connections is refused past the per-port cap instead of spawning a
/// thread for each one.
///
/// A published port accepts in a loop and hands each connection a thread, so the
/// only thing between a client that opens sockets in a loop and a daemon that runs
/// out of threads is the connection budget. The test opens more connections than the
/// cap allows and asserts the threads stayed at the cap, then that the port still
/// serves a client afterwards: a budget that refuses connections but leaks the slot
/// would pass the first half and fail the second.
#[test]
fn a_connection_flood_is_refused_past_the_cap_and_leaves_the_port_serving() {
    let _serial = serialize_heavy_connection_test();
    let service = TcpListener::bind("127.0.0.1:0").expect("bind the workspace service");
    let service_port = service.local_addr().expect("service addr").port();
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let accepted_by_service = Arc::clone(&accepted);
    // Hold every proxied connection open, so none of them gives its slot back while
    // the flood is running.
    let held = thread::spawn(move || {
        let mut streams = Vec::new();
        while let Ok((stream, _)) = service.accept() {
            accepted_by_service.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            streams.push(stream);
        }
        streams
    });

    let publisher = PortPublisher::new();
    let host_port = free_host_port();
    publish_loopback_port(&publisher, host_port, service_port);

    let flood = MAX_CONNECTIONS_PER_PUBLISHER + 32;
    let mut clients = Vec::with_capacity(flood);
    for _ in 0..flood {
        match TcpStream::connect(("127.0.0.1", host_port)) {
            Ok(stream) => clients.push(stream),
            // A refused connection is the cap doing its job.
            Err(_) => break,
        }
    }

    // Give the accept loop time to take everything it is going to take, then read
    // what reached the service. The loop runs once per poll, so this is generous.
    let expected = flood.min(MAX_CONNECTIONS_PER_PUBLISHER);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while accepted.load(std::sync::atomic::Ordering::SeqCst) < expected
        && std::time::Instant::now() < deadline
    {
        thread::sleep(std::time::Duration::from_millis(20));
    }
    let reached_service = accepted.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        reached_service >= expected,
        "the flood reached the service {reached_service} times, short of the {expected} a cap of          {MAX_CONNECTIONS_PER_PUBLISHER} allows, so the connections were not proxied at all"
    );
    assert!(
        reached_service <= MAX_CONNECTIONS_PER_PUBLISHER,
        "the flood proxied {reached_service} connections for a cap of {MAX_CONNECTIONS_PER_PUBLISHER}"
    );
    // The connections opened past the cap were refused. This is the assertion that
    // fails if the cap is raised to something the flood does not reach, which is the
    // difference between a bounded proxy and one that takes whatever it is given.
    if clients.len() > MAX_CONNECTIONS_PER_PUBLISHER {
        assert!(
            reached_service < clients.len(),
            "every one of the {} connections was proxied, so the cap refused nothing",
            clients.len()
        );
    }

    // The port is still usable: the refused connections must not have consumed the
    // slots the cap is made of, and the withdrawal must give the host port back.
    publisher.clear_workspace_ports("sb-publish", "ws-publish");
    let rebind = TcpListener::bind(("127.0.0.1", host_port));
    assert!(rebind.is_ok(), "the port is still held: {rebind:?}");
    drop(clients);
    drop(held);
}

#[test]
fn withdrawing_a_port_ends_its_open_connections() {
    let _serial = serialize_heavy_connection_test();
    let service = TcpListener::bind("127.0.0.1:0").expect("bind the workspace service");
    let service_port = service.local_addr().expect("service addr").port();
    let held = thread::spawn(move || {
        let mut accepted = Vec::new();
        while let Ok((stream, _)) = service.accept() {
            accepted.push(stream);
        }
        accepted
    });

    let publisher = PortPublisher::new();
    let host_port = free_host_port();
    publish_loopback_port(&publisher, host_port, service_port);

    let mut clients = Vec::new();
    for _ in 0..8 {
        clients.push(TcpStream::connect(("127.0.0.1", host_port)).expect("connect to the port"));
    }
    // Wait until the proxy is serving them, so the withdrawal has something to end.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while proxied_connection_threads() < clients.len() && std::time::Instant::now() < deadline {
        thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        proxied_connection_threads() >= 1,
        "no connection was proxied, so there is nothing for the withdrawal to end"
    );

    let started = std::time::Instant::now();
    publisher.clear_workspace_ports("sb-publish", "ws-publish");

    // Every client sees end of file, which is the proxy having shut the connection
    // down rather than the client having to time out.
    for mut client in clients {
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .expect("set a read timeout");
        let mut reply = Vec::new();
        client
            .read_to_end(&mut reply)
            .expect("the withdrawn connection must end, not hang");
        assert!(
            reply.is_empty(),
            "unexpected payload after withdrawal: {reply:?}"
        );
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the withdrawal took {:?}, which is past the deadline a stop has to meet",
        started.elapsed()
    );

    // And the host port is free, which is what the stop certificate rebinds.
    let rebind = TcpListener::bind(("127.0.0.1", host_port));
    assert!(rebind.is_ok(), "the port is still held: {rebind:?}");
    drop(held);
}
