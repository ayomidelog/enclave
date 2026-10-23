use super::*;
use crate::workspace::{detached_count, WorkspaceProcess};

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
        processes: Vec::new(),
    };
    let json = serde_json::to_string(&entry).expect("serialize");
    assert!(json.contains("devbox"));
    assert!(json.contains("84921"));
}

#[test]
fn process_rows_expose_identity_and_tracked_job() {
    let process = WorkspaceProcess {
        pid: 42,
        ppid: 1,
        process_group: 42,
        command: "sleep 60".to_string(),
        age_secs: Some(30),
        cpu_percent: Some(2.5),
        rss_bytes: Some(8 * 1024 * 1024),
        threads: Some(1),
        detached: true,
        job_id: Some("job-1".to_string()),
    };
    let json = serde_json::to_value(&process).expect("serialize");
    assert_eq!(json["pid"], 42);
    assert_eq!(json["ppid"], 1);
    assert_eq!(json["process_group"], 42);
    assert_eq!(json["threads"], 1);
    assert_eq!(json["detached"], true);
    assert_eq!(json["job_id"], "job-1");
}

#[test]
fn a_process_outside_a_tracked_group_is_not_detached() {
    let process = WorkspaceProcess {
        pid: 43,
        ppid: 1,
        process_group: 43,
        command: "service".to_string(),
        age_secs: Some(5),
        cpu_percent: None,
        rss_bytes: None,
        threads: Some(4),
        detached: false,
        job_id: None,
    };
    assert_eq!(detached_count(&[process], 1), 0);
}

#[test]
fn process_entry_stopped_has_no_pid_or_uptime() {
    let entry = ProcessEntry {
        sandbox: "devbox".to_string(),
        workspace: "shell".to_string(),
        status: "stopped".to_string(),
        uptime: None,
        pid: None,
        processes: Vec::new(),
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
