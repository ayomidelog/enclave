use super::*;

use std::time::Instant;

#[test]
fn a_successful_command_returns_its_output() {
    let output = HostCommand::new("/bin/sh")
        .args(["-c", "printf hello"])
        .run_checked()
        .expect("run");
    assert!(output.success());
    assert_eq!(output.stdout_text(), "hello");
}

#[test]
fn a_non_zero_exit_reports_the_status_and_stderr() {
    let error = HostCommand::new("/bin/sh")
        .args(["-c", "echo trouble >&2; exit 3"])
        .run_checked()
        .expect_err("a failing command must not report success");
    let failure = error
        .downcast_ref::<HostCommandError>()
        .expect("the error must carry host-command detail");
    assert_eq!(failure.kind, HostCommandFailure::ExitStatus);
    assert_eq!(failure.status, Some(3));
    assert!(failure.stderr.contains("trouble"), "{}", failure.stderr);
    assert!(!failure.is_timeout());
    assert!(
        failure.describe().starts_with("/bin/sh -c"),
        "{}",
        failure.describe()
    );
}

#[test]
fn a_command_that_outlives_its_deadline_is_stopped() {
    let started = Instant::now();
    let error = HostCommand::new("/bin/sh")
        .args(["-c", "sleep 30"])
        .timeout(Duration::from_millis(200))
        .run()
        .expect_err("a command past its deadline must not report success");
    let failure = error
        .downcast_ref::<HostCommandError>()
        .expect("the error must carry host-command detail");
    assert_eq!(failure.kind, HostCommandFailure::TimedOut);
    assert!(failure.is_timeout());
    assert!(is_timeout(&error));
    // The deadline is what bounds the call, not the command's own runtime.
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the timeout must stop the command promptly, took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_timeout_stops_the_whole_process_group() {
    // The child starts a background process and then waits. Killing only the
    // direct child would leave the background process running and holding the
    // output pipes, so the reader would never see end of file.
    let started = Instant::now();
    let error = HostCommand::new("/bin/sh")
        .args(["-c", "(sleep 30) & sleep 30"])
        .timeout(Duration::from_millis(200))
        .run()
        .expect_err("a command past its deadline must not report success");
    assert!(is_timeout(&error));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the deadline must not wait for the descendants, took {:?}",
        started.elapsed()
    );
}

#[test]
fn output_beyond_the_cap_is_truncated_without_stalling_the_command() {
    // 200 KiB of output against a 1 KiB cap. The command has to keep writing
    // until it finishes, so the reader must drain what it does not keep.
    let output = HostCommand::new("/bin/sh")
        .args(["-c", "head -c 204800 /dev/zero | tr '\\0' 'x'"])
        .output_cap(1024)
        .timeout(Duration::from_secs(10))
        .run_checked()
        .expect("run");
    assert_eq!(output.stdout.len(), 1024);
    assert!(output.stdout.iter().all(|byte| *byte == b'x'));
}

#[test]
fn a_large_stdin_payload_is_delivered_without_deadlock() {
    // The payload is larger than a pipe buffer, so the writer has to run on its
    // own thread while the command reads.
    let payload = vec![b'y'; 512 * 1024];
    let output = HostCommand::new("/bin/sh")
        .args(["-c", "wc -c"])
        .stdin(payload.clone())
        .timeout(Duration::from_secs(10))
        .run_checked()
        .expect("run");
    assert_eq!(output.stdout_text().trim(), payload.len().to_string());
}

#[test]
fn a_missing_program_is_reported_as_a_spawn_failure() {
    let error = HostCommand::new("/nonexistent/enclave-hostcmd-test")
        .run()
        .expect_err("a missing program must not report success");
    let failure = error
        .downcast_ref::<HostCommandError>()
        .expect("the error must carry host-command detail");
    assert_eq!(failure.kind, HostCommandFailure::Spawn);
    assert!(!failure.is_timeout());
}

#[test]
fn a_configured_timeout_is_clamped_to_the_hard_bound() {
    let command = HostCommand::new("/bin/true").timeout(Duration::from_secs(999_999));
    assert_eq!(command.timeout, MAX_TIMEOUT);
}

#[test]
fn the_default_deadline_is_bounded() {
    assert!(DEFAULT_TIMEOUT <= MAX_TIMEOUT);
    assert!(!DEFAULT_TIMEOUT.is_zero());
}

#[test]
fn a_timeout_names_the_variable_that_raises_it() {
    // The host-command deadline is resolved for every command, so it is the one
    // a loaded host hits first. The message has to say which variable to raise
    // rather than only how long it waited.
    let error = HostCommandError {
        program: "ip".to_string(),
        args: vec!["-batch".to_string()],
        kind: HostCommandFailure::TimedOut,
        status: None,
        stderr: String::new(),
        timeout: Duration::from_secs(10),
    };
    let rendered = error.to_string();
    assert!(
        rendered.contains(crate::deadlines::host_command().variable),
        "the message should name the override: {rendered}"
    );
    assert!(rendered.contains("did not finish within"), "{rendered}");
}
