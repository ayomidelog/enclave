//! The connection budget, and what a poisoned publisher lock does to it.

use super::*;

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
