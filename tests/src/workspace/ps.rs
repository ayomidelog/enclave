use super::*;

#[test]
fn format_uptime_minutes_only() {
    assert_eq!(format_uptime(0), "0m");
    assert_eq!(format_uptime(59), "0m");
    assert_eq!(format_uptime(60), "1m");
    assert_eq!(format_uptime(300), "5m");
}

#[test]
fn format_uptime_hours_and_minutes() {
    assert_eq!(format_uptime(3600), "1h 0m");
    assert_eq!(format_uptime(8040), "2h 14m");
    assert_eq!(format_uptime(7200 + 47 * 60), "2h 47m");
}

#[test]
fn format_uptime_days() {
    assert_eq!(format_uptime(86400), "1d 0h 0m");
    assert_eq!(format_uptime(90061), "1d 1h 1m");
    assert_eq!(format_uptime(172800 + 3600 + 120), "2d 1h 2m");
}

#[test]
fn process_entry_serializes_to_json() {
    let entry = ProcessEntry {
        sandbox: "devbox".to_string(),
        workspace: "api".to_string(),
        status: "running".to_string(),
        uptime: Some(8040),
        pid: Some(84921),
    };
    let json = serde_json::to_string(&entry).expect("serialize");
    assert!(json.contains("devbox"));
    assert!(json.contains("84921"));
}

#[test]
fn process_entry_stopped_has_no_pid_or_uptime() {
    let entry = ProcessEntry {
        sandbox: "devbox".to_string(),
        workspace: "shell".to_string(),
        status: "stopped".to_string(),
        uptime: None,
        pid: None,
    };
    let json = serde_json::to_value(&entry).expect("serialize");
    assert!(json["pid"].is_null());
    assert!(json["uptime"].is_null());
}

#[test]
fn system_uptime_is_readable() {
    let _ = system_uptime_secs();
}

#[test]
fn clock_ticks_returns_positive_value() {
    let ticks = clock_ticks_per_second();

    if let Some(t) = ticks {
        assert!(t > 0);
    }
}
