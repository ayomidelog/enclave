use enclave::workspace::{JobMode, JobRecord, JobStatus};
use std::os::unix::process::CommandExt;

fn temp_state(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "enclave-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn job_records_round_trip_with_mode_and_identity() {
    let state = temp_state("job-round-trip");
    let mut job = JobRecord::new(
        "sb-1",
        "ws-1",
        JobMode::Background,
        &["sleep".to_string(), "60".to_string()],
    );
    job.pid = Some(1234);
    job.starttime_ticks = Some(99);
    job.process_group = Some(1234);
    job.ttl_secs = Some(30);
    job.keep_running = true;
    enclave::workspace::jobs::persist(&state, &job).unwrap();

    let loaded = enclave::workspace::jobs::load(&state, &job.id).unwrap();
    assert_eq!(loaded.mode, JobMode::Background);
    assert_eq!(loaded.status, JobStatus::Running);
    assert_eq!(loaded.pid, Some(1234));
    assert_eq!(loaded.starttime_ticks, Some(99));
    assert_eq!(loaded.process_group, Some(1234));
    assert_eq!(loaded.command_line(), "sleep 60");

    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn terminal_state_is_not_overwritten_by_a_racing_supervisor() {
    let state = temp_state("job-terminal");
    let job = JobRecord::new(
        "sb-1",
        "ws-1",
        JobMode::Background,
        &["sleep".to_string(), "60".to_string()],
    );
    enclave::workspace::jobs::persist(&state, &job).unwrap();
    assert!(enclave::workspace::jobs::finish_if_running(
        &state,
        &job.id,
        JobStatus::Cancelled,
        None,
        Some("ttl".to_string()),
    )
    .unwrap());
    assert!(!enclave::workspace::jobs::finish_if_running(
        &state,
        &job.id,
        JobStatus::Failed,
        Some(143),
        None,
    )
    .unwrap());

    let loaded = enclave::workspace::jobs::load(&state, &job.id).unwrap();
    assert_eq!(loaded.status, JobStatus::Cancelled);
    assert_eq!(loaded.error.as_deref(), Some("ttl"));

    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn workspace_job_listing_is_scoped_by_sandbox_and_workspace() {
    let state = temp_state("job-scope");
    let matching = JobRecord::new("sb-1", "ws-1", JobMode::Foreground, &["true".to_string()]);
    let other_sandbox = JobRecord::new("sb-2", "ws-1", JobMode::Foreground, &["true".to_string()]);
    let other_workspace =
        JobRecord::new("sb-1", "ws-2", JobMode::Foreground, &["true".to_string()]);
    for job in [&matching, &other_sandbox, &other_workspace] {
        enclave::workspace::jobs::persist(&state, job).unwrap();
    }

    let listed = enclave::workspace::jobs::list_for_workspace(&state, "sb-1", "ws-1").unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, matching.id);

    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn zero_ttl_means_no_expiry() {
    let mut job = JobRecord::new("sb", "ws", JobMode::Background, &["true".to_string()]);
    job.ttl_secs = Some(0);
    assert!(!job.expired(chrono::Utc::now() + chrono::Duration::days(1)));
}

#[test]
fn cancelling_a_workspace_only_rewrites_jobs_that_are_still_running() {
    let state = temp_state("job-cancel-scope");
    let mut finished = JobRecord::new("sb", "ws", JobMode::Foreground, &["true".to_string()]);
    finished.status = JobStatus::Exited;
    finished.exit_code = Some(0);
    let running = JobRecord::new("sb", "ws", JobMode::Background, &["sleep".to_string()]);
    for job in [&finished, &running] {
        enclave::workspace::jobs::persist(&state, job).unwrap();
    }

    let changed = enclave::workspace::jobs::cancel_running_for_workspace(
        &state,
        "sb",
        "ws",
        "workspace stopped",
    )
    .unwrap();
    assert_eq!(changed, 1);
    assert_eq!(
        enclave::workspace::jobs::load(&state, &finished.id)
            .unwrap()
            .status,
        JobStatus::Exited
    );
    assert_eq!(
        enclave::workspace::jobs::load(&state, &running.id)
            .unwrap()
            .status,
        JobStatus::Cancelled
    );
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn an_expired_job_escalates_from_term_to_kill() {
    let state = temp_state("job-ttl-escalation");
    let mut command = std::process::Command::new("sh");
    command
        .args(["-c", "trap '' TERM; exec sleep 30"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: `setpgid` is async-signal-safe and runs before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let mut child = command.spawn().expect("spawn a TERM-resistant job");
    let pid = child.id();
    let mut job = JobRecord::new(
        "sb",
        "ws",
        JobMode::Background,
        &["sleep".to_string(), "30".to_string()],
    );
    job.pid = Some(pid);
    job.starttime_ticks = Some(process_starttime_ticks(pid));
    job.process_group = Some(i32::try_from(pid).expect("pid fits an i32"));
    job.started_at = "2020-01-01T00:00:00Z".to_string();
    job.ttl_secs = Some(1);
    enclave::workspace::jobs::persist(&state, &job).expect("persist expired job");

    let service = enclave::workspace::ActivityService::new();
    let cancelled = service
        .reap_expired_jobs(&state, chrono::Utc::now())
        .expect("reap expired job");
    assert_eq!(cancelled, vec![("ws".to_string(), job.id.clone())]);
    let loaded = enclave::workspace::jobs::load(&state, &job.id).expect("load job");
    assert_eq!(loaded.status, JobStatus::Cancelled);
    assert_eq!(loaded.error.as_deref(), Some("background job TTL expired"));
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&state);
}

fn process_starttime_ticks(pid: u32) -> u64 {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("read process stat");
    let rest = raw
        .rsplit_once(") ")
        .map(|(_, rest)| rest)
        .expect("parse process stat");
    rest.split_whitespace()
        .nth(19)
        .expect("starttime field")
        .parse()
        .expect("parse starttime")
}
