use super::*;

use std::time::Duration;

#[test]
fn an_unset_variable_keeps_the_default() {
    let deadline = Deadline::resolve_from(
        "probe",
        "ENCLAVE_TEST_DEADLINE",
        Unit::Milliseconds,
        Duration::from_millis(500),
        Duration::from_secs(10),
        None,
    );
    assert_eq!(deadline.get(), Duration::from_millis(500));
    assert!(!deadline.overridden);
}

#[test]
fn a_configured_value_replaces_the_default_and_is_marked_as_an_override() {
    let deadline = Deadline::resolve_from(
        "probe",
        "ENCLAVE_TEST_DEADLINE",
        Unit::Milliseconds,
        Duration::from_millis(500),
        Duration::from_secs(10),
        Some("2500"),
    );
    assert_eq!(deadline.get(), Duration::from_millis(2500));
    assert!(deadline.overridden);
}

#[test]
fn a_configured_value_is_clamped_to_the_hard_bound() {
    let deadline = Deadline::resolve_from(
        "probe",
        "ENCLAVE_TEST_DEADLINE",
        Unit::Seconds,
        Duration::from_secs(30),
        Duration::from_secs(600),
        Some("999999"),
    );
    assert_eq!(deadline.get(), Duration::from_secs(600));
    assert!(deadline.overridden);
}

#[test]
fn an_unusable_value_falls_back_to_the_default() {
    for raw in ["", "  ", "abc", "-1", "1.5", "0"] {
        let deadline = Deadline::resolve_from(
            "probe",
            "ENCLAVE_TEST_DEADLINE",
            Unit::Milliseconds,
            Duration::from_millis(500),
            Duration::from_secs(10),
            Some(raw),
        );
        assert_eq!(
            deadline.get(),
            Duration::from_millis(500),
            "value {raw:?} must not change the deadline"
        );
        assert!(!deadline.overridden);
    }
}

#[test]
fn the_reported_value_carries_its_origin() {
    let deadline = Deadline::resolve_from(
        "runtime_term_grace",
        "ENCLAVE_RUNTIME_TERM_GRACE_MS",
        Unit::Milliseconds,
        Duration::from_millis(500),
        Duration::from_secs(30),
        Some("1000"),
    );
    let described = deadline.describe();
    assert_eq!(described["name"], "runtime_term_grace");
    assert_eq!(described["variable"], "ENCLAVE_RUNTIME_TERM_GRACE_MS");
    assert_eq!(described["value_ms"], 1000);
    assert_eq!(described["default_ms"], 500);
    assert_eq!(described["max_ms"], 30_000);
    assert_eq!(described["overridden"], true);
}

#[test]
fn every_reported_deadline_is_non_zero_and_within_its_bound() {
    let reported = describe_all();
    let entries = reported.as_array().expect("the report is an array");
    assert!(!entries.is_empty());
    for entry in entries {
        let value = entry["value_ms"].as_u64().expect("value_ms is a number");
        let max = entry["max_ms"].as_u64().expect("max_ms is a number");
        assert!(value > 0, "{} has a zero deadline", entry["name"]);
        assert!(
            value <= max,
            "{} is {}ms, past its {}ms bound",
            entry["name"],
            value,
            max
        );
    }
}
