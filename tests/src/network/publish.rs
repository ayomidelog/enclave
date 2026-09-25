use super::*;

/// The threads that copy proxied connections, with their voluntary context
/// switch counts.
///
/// A connection that is waiting for data either blocks once and stays blocked, or
/// wakes on a timer to re-check whether it should stop. Only the second behaviour
/// costs anything, and the kernel counts exactly one voluntary context switch per
/// block-and-wake, so the total is a precise measure of polling that does not
/// depend on wall time, CPU speed, or what other tests are doing. Process-wide
/// CPU time cannot be used here because the test binary runs tests in parallel.
///
/// The kernel truncates a thread name to fifteen characters, so the match is on
/// the truncated form; the accept thread is named after its port and cannot
/// collide with it.
fn connection_thread_switches() -> (usize, u64) {
    const NAME_PREFIX: &str = "enclave-port-co";
    let mut matched = 0usize;
    let mut switches = 0u64;
    let Ok(entries) = std::fs::read_dir("/proc/self/task") else {
        return (0, 0);
    };
    for entry in entries.flatten() {
        let Ok(name) = std::fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        if !name.trim_end().starts_with(NAME_PREFIX) {
            continue;
        }
        matched += 1;
        let Ok(status) = std::fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        for line in status.lines() {
            if let Some(value) = line.strip_prefix("voluntary_ctxt_switches:") {
                switches += value.trim().parse::<u64>().unwrap_or(0);
            }
        }
    }
    (matched, switches)
}

#[test]
fn an_idle_connection_costs_no_cpu() {
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

/// An ephemeral port on the loopback interface, released before the caller uses
/// it. The publisher binds by number rather than by handing back the socket, so
/// the test has to name a port up front.
fn free_host_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    listener.local_addr().expect("read the bound port").port()
}

fn publish_loopback_port(publisher: &PortPublisher, host_port: u16, service_port: u16) {
    // A runtime pid equal to this process is what tells the proxy to reach the
    // service over loopback instead of entering a workspace network namespace,
    // which is what makes a published port testable without root.
    let spec = PublishedPortSpec {
        host_ip: "127.0.0.1".to_string(),
        host_port,
        workspace_port: service_port,
        protocol: "tcp".to_string(),
    };
    let statuses = publisher
        .apply_workspace_ports_strict(
            "sb-publish",
            "ws-publish",
            std::process::id(),
            "127.0.0.1",
            std::slice::from_ref(&spec),
        )
        .expect("publish the port");
    assert_eq!(statuses.len(), 1);
    assert_eq!(
        statuses[0].state,
        crate::workspace::PublishedPortState::Active
    );
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

/// Poison `mutex` by panicking while holding it, the way a panicking request
/// would. Returns whether the poison took effect.
fn poison(mutex: &Mutex<impl Sized>) -> bool {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = mutex.lock().expect("first lock");
        panic!("simulated panic while holding the lock");
    }));
    std::panic::set_hook(previous);
    assert!(result.is_err(), "the simulated panic should propagate");
    mutex.is_poisoned()
}

#[test]
fn the_publisher_keeps_serving_after_a_poisoned_lock() {
    // A panic in one request must not cost the daemon its port handling. Without
    // recovery every later publish, unpublish, and status call would panic too,
    // and each panic would take down another request worker.
    let publisher = PortPublisher::new();
    assert!(poison(&publisher.inner));

    assert!(publisher.active_workspaces().is_empty());
    assert!(publisher.workspace_statuses("sb", "ws").is_empty());
    assert!(!publisher.has_active_workspace_ports("sb", "ws"));
    publisher.clear_workspace_ports("sb", "ws");
    // The strict apply path also has to survive the poison and report a normal
    // failure rather than panicking.
    assert!(publisher
        .apply_workspace_ports_strict("sb", "ws", 1, "10.200.0.99", &[])
        .is_ok());
}

#[test]
fn a_permit_still_releases_its_slot_after_a_poisoned_lock() {
    // A panic inside this destructor would abort the process when it happened
    // during unwinding, so the release has to be panic free.
    let limiter = Arc::new(ConnectionLimiter::new(1));
    let permit = limiter.try_acquire().expect("first permit");
    assert!(poison(limiter.state_mutex()));
    drop(permit);
    assert_eq!(limiter.active(), 0);
}

#[test]
fn connection_limiter_rejects_above_configured_capacity() {
    let limiter = Arc::new(ConnectionLimiter::new(1));
    let first = limiter.try_acquire().expect("first permit");
    assert!(limiter.try_acquire().is_none());
    assert_eq!(limiter.active(), 1);
    drop(first);
    assert_eq!(limiter.active(), 0);
    assert!(limiter.try_acquire().is_some());
}

#[test]
fn connection_budget_holds_both_the_port_and_daemon_slots() {
    let publisher = Arc::new(ConnectionLimiter::new(1));
    let global = Arc::new(ConnectionLimiter::new(2));
    let budget = ConnectionBudget::new(Arc::clone(&publisher), Arc::clone(&global));

    let first = budget.try_acquire().expect("first permit");
    assert_eq!(publisher.active(), 1);
    assert_eq!(global.active(), 1);

    // The per-port budget is the tighter one here, so a second connection on
    // the same port is refused and does not touch the daemon-wide budget.
    assert!(budget.try_acquire().is_none());
    assert_eq!(global.active(), 1);

    drop(first);
    assert_eq!(publisher.active(), 0);
    assert_eq!(global.active(), 0);
}

#[test]
fn connection_budget_releases_the_port_slot_when_the_daemon_is_full() {
    let publisher = Arc::new(ConnectionLimiter::new(4));
    let global = Arc::new(ConnectionLimiter::new(1));
    let budget = ConnectionBudget::new(Arc::clone(&publisher), Arc::clone(&global));

    let held = global.try_acquire().expect("global permit");
    assert!(budget.try_acquire().is_none());
    // The refused connection must not leave a per-port slot behind.
    assert_eq!(publisher.active(), 0);

    drop(held);
    assert!(budget.try_acquire().is_some());
}

/// A reader that never produces data and never reaches end of file.
struct SilentReader;

impl Read for SilentReader {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::WouldBlock, "silent"))
    }
}

/// A reader that yields one byte and then goes silent.
struct OneByteThenSilent {
    remaining: usize,
}

impl Read for OneByteThenSilent {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || buffer.is_empty() {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "silent"));
        }
        self.remaining -= 1;
        buffer[0] = 7;
        Ok(1)
    }
}

#[test]
fn copy_until_shutdown_stops_without_waiting_for_eof() {
    let shutdown = AtomicBool::new(true);
    let activity = ConnectionActivity::new();
    let mut reader = std::io::Cursor::new(vec![1u8, 2, 3]);
    let mut output = Vec::new();
    copy_until_shutdown(
        &mut reader,
        &mut output,
        &shutdown,
        &activity,
        Duration::from_secs(60),
    )
    .expect("copy");
    assert!(output.is_empty());
}

#[test]
fn copy_until_shutdown_copies_available_data() {
    let shutdown = AtomicBool::new(false);
    let activity = ConnectionActivity::new();
    let mut reader = std::io::Cursor::new(vec![1u8, 2, 3]);
    let mut output = Vec::new();
    copy_until_shutdown(
        &mut reader,
        &mut output,
        &shutdown,
        &activity,
        Duration::from_secs(60),
    )
    .expect("copy");
    assert_eq!(output, vec![1, 2, 3]);
}

#[test]
fn copy_until_shutdown_closes_a_connection_that_goes_idle() {
    let shutdown = AtomicBool::new(false);
    let activity = ConnectionActivity::new();
    let mut reader = SilentReader;
    let mut output = Vec::new();
    let idle_timeout = Duration::from_millis(30);

    let started = std::time::Instant::now();
    copy_until_shutdown(&mut reader, &mut output, &shutdown, &activity, idle_timeout)
        .expect("copy");
    let elapsed = started.elapsed();

    assert!(output.is_empty());
    // The activity clock starts a hair before this test's timer, so allow a
    // small slack rather than comparing the two clocks exactly.
    assert!(
        elapsed >= idle_timeout.saturating_sub(Duration::from_millis(5)),
        "closed before the idle timeout: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "took too long: {elapsed:?}"
    );
}

#[test]
fn copy_until_shutdown_keeps_an_active_connection_open() {
    let shutdown = AtomicBool::new(false);
    let activity = ConnectionActivity::new();
    let mut reader = OneByteThenSilent { remaining: 1 };
    let mut output = Vec::new();

    // The single byte resets the idle clock, so the byte is copied before the
    // connection is reaped for going quiet.
    copy_until_shutdown(
        &mut reader,
        &mut output,
        &shutdown,
        &activity,
        Duration::from_millis(30),
    )
    .expect("copy");
    assert_eq!(output, vec![7]);
}
