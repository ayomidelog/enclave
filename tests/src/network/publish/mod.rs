//! Tests for the published port proxy.
//!
//! The fixtures are here because the submodules share them. They are about the host
//! rather than about one test: an ephemeral port, a publisher serving a loopback
//! service, and the process-wide thread and context-switch counts that say whether a
//! connection is costing anything while it waits.

use super::*;

/// Serializes the tests that open many proxied connections.
///
/// The connection count and the context-switch count are properties of the process,
/// not of one publisher, so two of these tests running at once would each read the
/// other's threads and fail for a reason that has nothing to do with the code.
static HEAVY_CONNECTION_TESTS: Mutex<()> = Mutex::new(());

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

/// An ephemeral port on the loopback interface, released before the caller uses
/// it. The publisher binds by number rather than by handing back the socket, so
/// the test has to name a port up front.
fn free_host_port() -> u16 {
    free_host_ports(1)[0]
}

/// Distinct free host ports.
///
/// Asking for one port at a time returns a port the kernel has just been told to
/// release, so a second call can be handed the same number. Holding every listener
/// until they have all been read is what makes the ports distinct, which a test
/// that publishes several needs.
fn free_host_ports(count: usize) -> Vec<u16> {
    let listeners = (0..count)
        .map(|_| TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port"))
        .collect::<Vec<_>>();
    listeners
        .iter()
        .map(|listener| listener.local_addr().expect("read the bound port").port())
        .collect()
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

fn serialize_heavy_connection_test() -> std::sync::MutexGuard<'static, ()> {
    HEAVY_CONNECTION_TESTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

/// The connection threads a published port is serving right now.
fn proxied_connection_threads() -> usize {
    connection_thread_switches().0
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

mod copy;
mod limiter;
mod load;
mod proxy;
