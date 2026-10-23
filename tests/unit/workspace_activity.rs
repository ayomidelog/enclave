use chrono::{Duration, Utc};
use enclave::workspace::{WorkspaceMetadata, WorkspaceStatus};

fn workspace() -> WorkspaceMetadata {
    WorkspaceMetadata {
        id: "ws-activity".to_string(),
        sandbox_id: "sb-activity".to_string(),
        name: "activity".to_string(),
        created_at: "2026-08-06T00:00:00Z".to_string(),
        last_request_at: None,
        last_process_activity_at: None,
        last_stopped_at: None,
        workspace_path: "/tmp/enclave-activity/ws".to_string(),
        filesystem_path: "/tmp/enclave-activity/ws/fs".to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: "/tmp/enclave-activity/rootfs".to_string(),
        overlay_home_base_path: "/tmp/enclave-activity/home-base".to_string(),
        overlay_home_upper_path: String::new(),
        overlay_home_work_path: String::new(),
        overlay_home_merged_path: String::new(),
        auth_providers: Vec::new(),
        owner: None,
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: WorkspaceStatus::Running,
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: Default::default(),
        clear_tmp_on_restart: false,
        keep_running: false,
        idle_stop_after_secs: Some(60),
        destroy_after_secs: None,
        limits: Default::default(),
        assigned_ip: None,
    }
}

#[test]
fn last_activity_uses_the_newer_timestamp() {
    let now = Utc::now();
    let mut workspace = workspace();
    workspace.last_request_at = Some((now - Duration::seconds(90)).to_rfc3339());
    workspace.last_process_activity_at = Some((now - Duration::seconds(10)).to_rfc3339());

    assert_eq!(enclave::workspace::idle_for_secs(&workspace, now), Some(10));
}

#[test]
fn idle_requires_running_unpinned_workspace_without_a_lease() {
    let now = Utc::now();
    let mut workspace = workspace();
    workspace.last_request_at = Some((now - Duration::seconds(120)).to_rfc3339());

    assert!(enclave::workspace::is_idle(&workspace, 60, now, 0));
    assert!(!enclave::workspace::is_idle(&workspace, 60, now, 1));

    workspace.keep_running = true;
    assert!(!enclave::workspace::is_idle(&workspace, 60, now, 0));

    workspace.keep_running = false;
    workspace.status = WorkspaceStatus::Stopped;
    assert!(!enclave::workspace::is_idle(&workspace, 60, now, 0));
}

#[test]
fn zero_idle_window_disables_idle_stop() {
    let now = Utc::now();
    let mut workspace = workspace();
    workspace.last_request_at = Some((now - Duration::days(1)).to_rfc3339());

    assert!(!enclave::workspace::is_idle(&workspace, 0, now, 0));
}

#[test]
fn workspace_thresholds_override_daemon_defaults() {
    let mut workspace = workspace();
    workspace.idle_stop_after_secs = Some(15);
    workspace.destroy_after_secs = None;

    assert_eq!(
        enclave::workspace::idle_threshold(&workspace, Some(60)),
        Some(15)
    );
    assert_eq!(
        enclave::workspace::destroy_threshold(&workspace, Some(300)),
        Some(300)
    );
}

#[test]
fn stopping_records_a_timestamp_without_changing_activity() {
    let mut workspace = workspace();
    workspace.last_request_at = Some("2026-08-06T00:00:00Z".to_string());
    enclave::workspace::mark_stopped(&mut workspace);

    assert_eq!(
        workspace.last_request_at.as_deref(),
        Some("2026-08-06T00:00:00Z")
    );
    assert!(workspace.last_stopped_at.is_some());
}

#[test]
fn status_report_carries_policy_and_activity_fields() {
    let report = enclave::workspace::WorkspaceStatusReport {
        id: "ws".to_string(),
        name: "workspace".to_string(),
        created_at: "2026-08-06T00:00:00Z".to_string(),
        allocated_path: "/tmp/ws".to_string(),
        status: WorkspaceStatus::Running,
        process_count: 1,
        task_count: Some(4),
        resource_usage: None,
        last_request_at: Some("2026-08-06T00:00:01Z".to_string()),
        last_process_activity_at: Some("2026-08-06T00:00:02Z".to_string()),
        last_stopped_at: Some("2026-08-06T00:00:03Z".to_string()),
        keep_running: true,
        idle_stop_after_secs: Some(60),
        destroy_after_secs: Some(600),
        limits: Default::default(),
        sandbox_limits: Default::default(),
        published_ports: Vec::new(),
    };
    let json = serde_json::to_value(&report).expect("serialize status report");
    assert_eq!(json["task_count"], 4);
    assert_eq!(json["keep_running"], true);
    assert_eq!(json["idle_stop_after_secs"], 60);
    assert_eq!(json["last_stopped_at"], "2026-08-06T00:00:03Z");
}

#[test]
fn daemon_defaults_fill_only_omitted_workspace_limits() {
    let limits =
        enclave::workspace::WorkspaceLimits::default().apply_defaults(Some(25.0), Some(64));
    assert_eq!(limits.cpu_percent, Some(25.0));
    assert_eq!(limits.max_processes, Some(64));

    let declared = enclave::workspace::WorkspaceLimits {
        cpu_percent: Some(10.0),
        max_processes: Some(8),
        ..Default::default()
    }
    .apply_defaults(Some(25.0), Some(64));
    assert_eq!(declared.cpu_percent, Some(10.0));
    assert_eq!(declared.max_processes, Some(8));
}
