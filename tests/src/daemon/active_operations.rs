use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::*;

#[test]
fn a_running_operation_is_reported_until_its_guard_drops() {
    let active = Arc::new(ActiveOperations::default());
    assert!(active.in_flight().is_empty());

    let guard = active.begin("op-1", "workspace.start", "sb/ws");
    let in_flight = active.in_flight();
    assert_eq!(in_flight.len(), 1);
    assert_eq!(in_flight[0].id, "op-1");
    assert_eq!(in_flight[0].action, "workspace.start");
    assert_eq!(in_flight[0].target, "sb/ws");

    drop(guard);
    assert!(active.in_flight().is_empty());
}

#[test]
fn draining_an_idle_registry_returns_at_once() {
    let active = Arc::new(ActiveOperations::default());
    let started = std::time::Instant::now();
    let remaining = active.wait_for_drain(Duration::from_secs(30));
    assert!(remaining.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "an idle registry must not wait for the whole grace period"
    );
}

#[test]
fn draining_reports_the_operations_that_outlive_the_grace_period() {
    let active = Arc::new(ActiveOperations::default());
    let guard = active.begin("op-slow", "workspace.stop", "sb/ws");

    let started = std::time::Instant::now();
    let remaining = active.wait_for_drain(Duration::from_millis(50));
    let elapsed = started.elapsed();

    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, "op-slow");
    assert!(
        elapsed >= Duration::from_millis(45),
        "the wait should use the grace period before giving up: {elapsed:?}"
    );
    drop(guard);
}

#[test]
fn draining_returns_as_soon_as_the_last_operation_finishes() {
    let active = Arc::new(ActiveOperations::default());
    let guard = active.begin("op-quick", "workspace.start", "sb/ws");

    let finisher = thread::spawn(move || {
        thread::sleep(Duration::from_millis(40));
        drop(guard);
    });

    let started = std::time::Instant::now();
    let remaining = active.wait_for_drain(Duration::from_secs(30));
    let elapsed = started.elapsed();

    assert!(remaining.is_empty());
    assert!(
        elapsed < Duration::from_secs(5),
        "the wait should end with the operation, not the grace period: {elapsed:?}"
    );
    finisher.join().expect("finisher thread");
}

#[test]
fn a_described_operation_carries_the_fields_a_report_needs() {
    let active = Arc::new(ActiveOperations::default());
    let guard = active.begin("op-2", "sandbox.destroy", "sb");
    let described = active.in_flight()[0].describe();
    assert_eq!(described["operation_id"], "op-2");
    assert_eq!(described["action"], "sandbox.destroy");
    assert_eq!(described["target"], "sb");
    assert!(described["elapsed_secs"].is_u64());
    drop(guard);
}
