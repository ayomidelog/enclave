use super::*;

#[test]
fn hostname_lowercases_name() {
    assert_eq!(workspace_runtime_hostname("MyProject"), "myproject");
}

#[test]
fn hostname_replaces_special_chars_with_dash() {
    assert_eq!(
        workspace_runtime_hostname("my project!here"),
        "my-project-here"
    );
}

#[test]
fn hostname_collapses_consecutive_dashes() {
    assert_eq!(workspace_runtime_hostname("hello---world"), "hello-world");
}

#[test]
fn hostname_trims_leading_trailing_dashes() {
    assert_eq!(workspace_runtime_hostname("--hello--"), "hello");
}

#[test]
fn hostname_truncates_to_63_chars() {
    let long_name = "a".repeat(100);
    let result = workspace_runtime_hostname(&long_name);
    assert!(result.len() <= 63);
}

#[test]
fn hostname_empty_name_returns_workspace() {
    assert_eq!(workspace_runtime_hostname(""), "workspace");
}

#[test]
fn hostname_all_special_returns_workspace() {
    assert_eq!(workspace_runtime_hostname("!!!"), "workspace");
}

#[test]
fn process_alive_returns_true_for_self() {
    assert!(process_alive(std::process::id()));
}

#[test]
fn process_alive_returns_false_for_impossible_pid() {
    assert!(!process_alive(u32::MAX));
}

#[test]
fn process_matches_returns_false_for_dead_process() {
    assert!(!process_matches(u32::MAX, None));
}

/// A runtime that exited but has not been reaped still has a `/proc` entry.
/// Treating it as alive made workspace stops fail with "did not exit after
/// SIGKILL" whenever the daemon had not yet waited on its child.
#[test]
fn process_matches_returns_false_for_unreaped_child() {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn short-lived child");
    let pid = child.id();
    let starttime = process_starttime_ticks(pid).expect("child start time");

    // Wait for the child to exit without reaping it, then confirm it is still
    // visible in /proc while `process_matches` reports it as gone.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while process_starttime_ticks(pid).is_ok() && !process_is_zombie(pid) {
        assert!(
            std::time::Instant::now() < deadline,
            "child never reached the zombie state"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert!(process_alive(pid), "unreaped child should still be visible");
    assert!(
        !process_matches(pid, Some(starttime)),
        "an exited but unreaped runtime must not be reported as running"
    );
    let _ = child.wait();
    assert!(!process_matches(pid, Some(starttime)));
}

fn process_is_zombie(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|raw| raw.split_whitespace().nth(2).map(|state| state == "Z"))
        .unwrap_or(false)
}

#[test]
fn starttime_ticks_succeeds_for_self() {
    let result = process_starttime_ticks(std::process::id());
    assert!(result.is_ok(), "expected Ok, got: {:?}", result);
}

#[test]
fn parse_status_kb_value_extracts_number() {
    assert_eq!(parse_status_kb_value("   1234 kB"), Some(1234));
}

#[test]
fn parse_status_kb_value_returns_none_for_empty() {
    assert_eq!(parse_status_kb_value(""), None);
}

#[test]
fn parse_status_kb_value_returns_none_for_non_numeric() {
    assert_eq!(parse_status_kb_value("abc kB"), None);
}

#[test]
fn runtime_cmdline_detection_accepts_bootstrap_helper() {
    assert!(looks_like_enclave_runtime_cmdline(
        "enclave internal workspace-session-bootstrap --rootfs /tmp/rootfs --ready-file /tmp/ready"
    ));
}

#[test]
fn runtime_cmdline_detection_rejects_unrelated_process() {
    assert!(!looks_like_enclave_runtime_cmdline("/usr/bin/bash -lc env"));
}

#[test]
fn a_live_pid_that_is_not_an_enclave_runtime_is_reported_as_stale() {
    // This test binary is alive and owned by the current user, but its command
    // line is not an Enclave runtime, so the record naming it is stale.
    let target = verify_signal_target(std::process::id(), None).expect("inspect self");
    assert!(matches!(
        target,
        SignalTarget::Stale(StaleTarget::NotEnclaveProcess)
    ));
}

#[test]
fn a_pid_whose_start_time_does_not_match_is_signallable() {
    // A reused pid is not the process the record described, so there is nothing
    // to refuse: the recorded process is already gone.
    let target = verify_signal_target(std::process::id(), Some(0)).expect("inspect self");
    assert!(target.is_signallable());
}

#[test]
fn a_pid_that_no_longer_exists_is_signallable() {
    let target = verify_signal_target(u32::MAX, None).expect("inspect missing pid");
    assert!(target.is_signallable());
}
