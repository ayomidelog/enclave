use std::process::Command;

mod common;

#[test]
fn ps_help_is_available() {
    let output = Command::new(common::enclave_bin())
        .args(["ps", "--help"])
        .output()
        .expect("failed to run enclave ps --help");

    assert!(
        output.status.success(),
        "ps --help should succeed: {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage") || stdout.contains("ps"),
        "help should show ps usage: {stdout}"
    );
    assert!(
        stdout.contains("--local") && stdout.contains("--project"),
        "help should include local/project flags: {stdout}"
    );
}

#[test]
fn ps_project_alias_does_not_panic() {
    let output = Command::new(common::enclave_bin())
        .args(["ps", "--project"])
        .output()
        .expect("failed to run enclave ps --project");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{stderr}");
    assert!(
        !stderr.contains("panicked"),
        "ps --project should not panic: {stderr}"
    );
    assert!(
        !combined.contains("unexpected argument '--project'"),
        "--project alias should be accepted by CLI parser: {combined}"
    );
}

#[test]
fn ps_is_listed_in_top_level_help() {
    let output = Command::new(common::enclave_bin())
        .args(["--help"])
        .output()
        .expect("failed to run enclave --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("ps"),
        "top-level help should list ps command: {stdout}"
    );
}

#[test]
fn ps_without_daemon_does_not_panic() {
    let output = Command::new(common::enclave_bin())
        .args(["ps"])
        .output()
        .expect("failed to run enclave ps");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "ps should not panic: {stderr}"
    );
}

#[test]
fn ps_requires_root() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    let output = Command::new(common::enclave_bin())
        .args(["ps"])
        .output()
        .expect("failed to run enclave ps");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("root") || combined.contains("privilege"),
        "should mention root requirement: {combined}"
    );
}

#[test]
fn ps_sequential_calls_are_stable() {
    for i in 0..3 {
        let output = Command::new(common::enclave_bin())
            .args(["ps"])
            .output()
            .unwrap_or_else(|e| panic!("iteration {i}: {e}"));

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("panicked"),
            "iteration {i}: should not panic: {stderr}"
        );
    }
}
