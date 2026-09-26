use super::{
    infer_workspace_helper_from_current_exe, launch_userns_args, log_reports_text_file_busy,
    resolve_session_helper_source, session_helper_load_failure, session_helper_path,
    setgroups_args, stop_sessions_batch,
    userns::{IdMapRange, UserNamespaceMode, UserNamespacePlan},
};

/// Write a session log and return its path.
fn session_log(tag: &str, contents: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("enclave-session-log-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create log dir");
    let path = dir.join("session.log");
    std::fs::write(&path, contents).expect("write log");
    path
}

#[test]
fn a_helper_that_cannot_load_names_the_library_and_the_memory_limit() {
    // A workspace memory limit smaller than the helper needs makes the dynamic
    // loader fail with a message that names neither the limit nor the fact that
    // the limit is the cause, which is what the operator has to change.
    let log = session_log(
        "loader-limit",
        "workspace session bootstrap complete; switching to hardened runtime helper\n\
         /tmp/enclave/session-helper: error while loading shared libraries: libc.so.6: \
         failed to map segment from shared object\n",
    );
    let message = session_helper_load_failure(&log, Some(20 * 1024 * 1024))
        .expect("the loader failure is recognized");
    assert!(message.contains("libc.so.6"), "{message}");
    assert!(message.contains("memory_mb = 20"), "{message}");
    assert!(message.contains("raise the limit"), "{message}");
    let _ = std::fs::remove_dir_all(log.parent().expect("log parent"));
}

#[test]
fn a_helper_that_cannot_load_without_a_limit_names_the_host_library() {
    let log = session_log(
        "loader-no-limit",
        "/usr/local/bin/enclave: error while loading shared libraries: \
         libc.so.6: version `GLIBC_2.39' not found\n",
    );
    let message = session_helper_load_failure(&log, None).expect("recognized");
    assert!(message.contains("libc.so.6"), "{message}");
    assert!(message.contains("missing a shared library"), "{message}");
    assert!(!message.contains("memory_mb"), "{message}");
    let _ = std::fs::remove_dir_all(log.parent().expect("log parent"));
}

#[test]
fn a_healthy_session_log_is_not_a_load_failure() {
    let log = session_log(
        "healthy",
        "workspace session bootstrap starting\nworkspace session ready\n",
    );
    assert!(session_helper_load_failure(&log, Some(64 * 1024 * 1024)).is_none());
    // A log that does not exist yet is the normal state before the helper writes
    // anything, so it must not be reported as a failure either.
    assert!(session_helper_load_failure(&log.with_extension("absent"), None).is_none());
    let _ = std::fs::remove_dir_all(log.parent().expect("log parent"));
}
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn workspace_fixture() -> super::super::types::WorkspaceMetadata {
    super::super::types::WorkspaceMetadata {
        id: "ws-123".to_string(),
        sandbox_id: "sb-123".to_string(),
        name: "dev".to_string(),
        created_at: "2026-03-11T00:00:00Z".to_string(),
        workspace_path: "/tmp/enclave-test/sandboxes/sb-123/workspaces/ws-123".to_string(),
        filesystem_path: "/tmp/enclave-test/sandboxes/sb-123/workspaces/ws-123/fs".to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: "/tmp/enclave-test/rootfs".to_string(),
        overlay_home_base_path: "/tmp/enclave-test/home-base".to_string(),
        overlay_home_upper_path: "/tmp/enclave-test/sandboxes/sb-123/workspaces/ws-123/home-upper"
            .to_string(),
        overlay_home_work_path: "/tmp/enclave-test/sandboxes/sb-123/workspaces/ws-123/home-work"
            .to_string(),
        overlay_home_merged_path:
            "/tmp/enclave-test/sandboxes/sb-123/workspaces/ws-123/home-merged".to_string(),
        auth_providers: Vec::new(),
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: Default::default(),
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: Default::default(),
        clear_tmp_on_restart: false,
        limits: Default::default(),
        assigned_ip: None,
    }
}

#[test]
fn setgroups_args_only_denies_setgroups_for_multi_id_mappings() {
    let subordinate = super::userns::UserNamespacePlan {
        owner: "runner".to_string(),
        uid_map: IdMapRange {
            inner_start: 0,
            outer_start: 100000,
            count: 65536,
        },
        gid_map: IdMapRange {
            inner_start: 0,
            outer_start: 100000,
            count: 65536,
        },
    };
    assert_eq!(setgroups_args(&subordinate), vec!["--deny-setgroups"]);

    let identity = super::userns::UserNamespacePlan {
        owner: "root".to_string(),
        uid_map: IdMapRange {
            inner_start: 0,
            outer_start: 0,
            count: 1,
        },
        gid_map: IdMapRange {
            inner_start: 0,
            outer_start: 0,
            count: 1,
        },
    };
    assert!(setgroups_args(&identity).is_empty());
}

#[test]
fn launch_userns_args_enable_mapping_and_setgroups_when_userns_is_enabled() {
    let plan = UserNamespacePlan {
        owner: "alice".to_string(),
        uid_map: IdMapRange {
            inner_start: 0,
            outer_start: 100_000,
            count: 65_536,
        },
        gid_map: IdMapRange {
            inner_start: 0,
            outer_start: 100_000,
            count: 65_536,
        },
    };
    let args = launch_userns_args(&UserNamespaceMode::Enabled(plan));
    assert!(args.contains(&"--enable-userns".to_string()));
    assert!(args.contains(&"--deny-setgroups".to_string()));
    assert!(args.contains(&"100000".to_string()));
    assert!(args.contains(&"65536".to_string()));
}

#[test]
fn launch_userns_args_omit_enable_flag_when_userns_is_disabled() {
    let args = launch_userns_args(&UserNamespaceMode::Disabled);
    assert!(!args.contains(&"--enable-userns".to_string()));
    assert!(!args.contains(&"--deny-setgroups".to_string()));
    assert!(args.contains(&"1".to_string()));
}

#[test]
fn session_helper_path_uses_procfs_exe_reference() {
    let workspace = workspace_fixture();
    assert_eq!(
        session_helper_path(&workspace),
        std::path::PathBuf::from("/tmp/enclave-test/sandboxes/sb-123/runtime/session-helper")
    );
}

#[test]
fn resolve_session_helper_source_prefers_override_env() {
    let exe = std::env::current_exe().expect("current exe");
    std::env::set_var("ENCLAVE_SELF_EXE", &exe);
    std::env::remove_var("CARGO_BIN_EXE_enclave");
    assert_eq!(resolve_session_helper_source(), exe);
    std::env::remove_var("ENCLAVE_SELF_EXE");
}

#[test]
fn infer_workspace_helper_from_current_exe_detects_target_debug_binary() {
    let temp = std::env::temp_dir().join(format!(
        "enclave-helper-infer-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let deps = temp.join("target/debug/deps");
    std::fs::create_dir_all(&deps).expect("create deps dir");
    let current_exe = deps.join("integration_suite-abcdef");
    let expected = temp.join("target/debug/enclave");
    std::fs::write(&expected, b"#!/bin/sh\n").expect("write enclave binary placeholder");

    assert_eq!(
        infer_workspace_helper_from_current_exe(&current_exe),
        Some(expected.clone())
    );

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn stop_sessions_batch_stops_multiple_term_resistant_processes_with_one_shared_timeout() {
    let mut children = vec![
        spawn_term_resistant_enclave_process(),
        spawn_term_resistant_enclave_process(),
    ];
    let targets = children
        .iter()
        .map(|child| {
            let pid = child.id();
            let starttime = super::process_starttime_ticks(pid).expect("process starttime");
            (pid, Some(starttime))
        })
        .collect::<Vec<_>>();

    let started = Instant::now();
    let result = stop_sessions_batch(&targets).expect("batch stop should succeed");
    let elapsed = started.elapsed();

    assert_eq!(
        result.stopped_pids.len() + result.failed_pids.len(),
        2,
        "every target pid should be accounted for in the batch result"
    );
    for child in &mut children {
        let _ = child.kill();
        let _ = child.wait();
    }

    assert!(
        elapsed < Duration::from_secs(6),
        "batch stop took too long: {:?}; expected one shared timeout window, not serial waits",
        elapsed
    );
}

fn spawn_term_resistant_enclave_process() -> Child {
    Command::new("bash")
        .args([
            "-lc",
            "exec -a enclave-workspace-session bash -lc 'trap \"\" TERM; while :; do sleep 1; done'",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn enclave-like process")
}

/// A stop that arrives while a runtime is still inside the `exec` that makes it
/// the runtime must wait for the command line to settle.
///
/// A process carries its launcher's command line from the fork until the exec
/// finishes, and while the exec is in progress its command line reads as empty.
/// Reading either as a record that no longer names a runtime is what made a stop
/// clear the record without signalling anything, leaving the runtime running with
/// nothing on the host naming it.
#[test]
fn a_stop_waits_for_a_runtime_that_is_still_inside_its_exec() {
    let (mut child, launcher_dir) = spawn_runtime_that_execs_after(Duration::from_millis(150));
    let pid = child.id();
    let starttime = super::process_starttime_ticks(pid).expect("process starttime");

    let result = stop_sessions_batch(&[(pid, Some(starttime))]).expect("batch stop");

    assert!(
        result.stopped_pids.contains(&pid),
        "the runtime the stop waited for should be reported stopped"
    );
    assert!(
        result.failed_pids.is_empty(),
        "no target should be left running: {:?}",
        result.failed_pids
    );
    assert!(
        reaped_within(&mut child, Duration::from_secs(5)),
        "the stop reported the runtime stopped while it was still running"
    );
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(launcher_dir);
}

/// A pid that never becomes a runtime is still resolved as a stale record, and
/// the stop does not signal it.
///
/// The settle wait is a deadline rather than a wait for the process to change, so
/// a record naming something that is not a runtime at all is cleared once the
/// deadline passes. The process itself is left alone: it is not this daemon's to
/// end.
#[test]
fn a_stop_gives_up_on_a_pid_that_never_becomes_a_runtime() {
    let mut child = Command::new("bash")
        .args(["-c", "sleep 300"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a process that is not a runtime");
    let pid = child.id();
    let starttime = super::process_starttime_ticks(pid).expect("process starttime");

    let result = stop_sessions_batch(&[(pid, Some(starttime))]).expect("batch stop");

    assert!(result.stopped_pids.contains(&pid));
    assert!(
        child.try_wait().expect("poll the child").is_none(),
        "a process that is not an enclave runtime must not be signalled"
    );
    let _ = child.kill();
    let _ = child.wait();
}

/// A process whose command line does not name a runtime yet, and becomes one
/// after `delay` by exec'ing in place.
///
/// The launcher is a script file rather than a `bash -c` string, because a string
/// would carry the runtime's name in the launcher's own command line and the window
/// this tests would not exist. The pid and its start time survive the exec, which is
/// what makes the process the same one the record named before and after the window.
fn spawn_runtime_that_execs_after(delay: Duration) -> (Child, std::path::PathBuf) {
    let launcher_dir = std::env::temp_dir().join(format!(
        "enclave-runtime-exec-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&launcher_dir).expect("create the launcher directory");
    let launcher = launcher_dir.join("launch.sh");
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nsleep {}\nexec -a enclave-workspace-session sleep 300\n",
            delay.as_secs_f64()
        ),
    )
    .expect("write the launcher");
    // Bash, because `exec -a` is what renames the process and it is not a POSIX
    // `sh` feature.
    let child = Command::new("bash")
        .arg(&launcher)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a runtime that execs after a delay");
    (child, launcher_dir)
}

/// Whether `child` exits within `timeout`.
fn reaped_within(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().expect("poll the child").is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// A launch that failed because the helper binary was momentarily open for
/// writing is the one launch failure worth retrying, and it is recognized from
/// the session log rather than from the exit status: the launcher is started
/// with setsid -f, which reports success as soon as it has forked.
#[test]
fn a_busy_helper_binary_is_recognized_from_the_session_log() {
    let busy = session_log(
        "txtbusy",
        "setsid: failed to execute /tmp/enclave/session-helper: Text file busy\n",
    );
    assert!(log_reports_text_file_busy(&busy));
    let _ = std::fs::remove_dir_all(busy.parent().expect("log parent"));
}

#[test]
fn another_launch_failure_is_not_mistaken_for_a_busy_helper_binary() {
    let other = session_log(
        "other-failure",
        "setsid: failed to execute /tmp/enclave/session-helper: Permission denied\n",
    );
    assert!(!log_reports_text_file_busy(&other));
    let _ = std::fs::remove_dir_all(other.parent().expect("log parent"));
}

#[test]
fn a_missing_session_log_is_not_a_busy_helper_binary() {
    let missing = std::env::temp_dir().join(format!(
        "enclave-session-log-missing-{}",
        std::process::id()
    ));
    assert!(!log_reports_text_file_busy(&missing.join("session.log")));
}
