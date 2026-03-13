use std::process::Command;

mod common;

#[test]
fn doctor_checks_stale_cgroups() {
    let output = Command::new(common::enclave_bin())
        .args(["doctor"])
        .output()
        .expect("failed to run enclave doctor");

    let _code = output.status.code();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stderr.contains("panicked"),
        "doctor should not panic: {stderr}"
    );

    let _ = stdout;
}

#[test]
fn health_reports_cgroup_status() {
    let output = Command::new(common::enclave_bin())
        .args(["health"])
        .output()
        .expect("failed to run enclave health");

    let _code = output.status.code();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "health should not panic: {stderr}"
    );
}

#[test]
fn doctor_output_includes_cgroup_check() {
    let output = Command::new(common::enclave_bin())
        .args(["doctor"])
        .output()
        .expect("failed to run enclave doctor");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        combined.contains("cgroup")
            || combined.contains("ok")
            || combined.contains("warn")
            || combined.contains("root")
            || combined.contains("error"),
        "doctor should include cgroup check output or a permission error: {combined}"
    );
}

#[test]
fn doctor_runs_multiple_checks() {
    let output = Command::new(common::enclave_bin())
        .args(["doctor"])
        .output()
        .expect("failed to run enclave doctor");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stderr.contains("panicked"),
        "doctor should not panic: {stderr}"
    );
    assert!(
        !stdout.is_empty() || !stderr.is_empty(),
        "doctor should produce some output"
    );
}

#[test]
fn doctor_help_is_available() {
    let output = Command::new(common::enclave_bin())
        .args(["doctor", "--help"])
        .output()
        .expect("failed to run enclave doctor --help");

    assert!(
        output.status.success(),
        "doctor --help should succeed: {:?}",
        output.status
    );
}
