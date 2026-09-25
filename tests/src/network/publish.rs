use super::*;

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
