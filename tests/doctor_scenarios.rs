use std::process::Command;

mod common;

#[test]
fn doctor_clean_system_no_panic() {
    let output = Command::new(common::enclave_bin())
        .args(["doctor"])
        .output()
        .expect("failed to run enclave doctor");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "doctor should not panic: {stderr}"
    );
}

#[test]
fn doctor_produces_output() {
    let output = Command::new(common::enclave_bin())
        .args(["doctor"])
        .output()
        .expect("failed to run enclave doctor");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let has_output = !stdout.is_empty() || !stderr.is_empty();
    assert!(has_output, "doctor should produce output");
}

#[test]
fn doctor_exit_code_is_deterministic() {
    let output1 = Command::new(common::enclave_bin())
        .args(["doctor"])
        .output()
        .expect("failed to run enclave doctor");

    let output2 = Command::new(common::enclave_bin())
        .args(["doctor"])
        .output()
        .expect("failed to run enclave doctor");

    assert_eq!(
        output1.status.code(),
        output2.status.code(),
        "doctor exit code should be deterministic"
    );
}

#[test]
fn health_is_read_only() {
    let output1 = Command::new(common::enclave_bin())
        .args(["health"])
        .output()
        .expect("failed to run enclave health");

    let output2 = Command::new(common::enclave_bin())
        .args(["health"])
        .output()
        .expect("failed to run enclave health");

    assert_eq!(
        output1.status.code(),
        output2.status.code(),
        "health should return same exit code on repeated calls"
    );
}

#[test]
fn doctor_handles_missing_state() {
    let unique_dir =
        std::env::temp_dir().join(format!("enclave-test-doctor-state-{}", std::process::id()));
    let output = Command::new(common::enclave_bin())
        .args(["doctor"])
        .env("XDG_STATE_HOME", &unique_dir)
        .output()
        .expect("failed to run enclave doctor");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "doctor should not panic with missing state: {stderr}"
    );
}

#[test]
fn health_handles_missing_state() {
    let unique_dir =
        std::env::temp_dir().join(format!("enclave-test-health-state-{}", std::process::id()));
    let output = Command::new(common::enclave_bin())
        .args(["health"])
        .env("XDG_STATE_HOME", &unique_dir)
        .output()
        .expect("failed to run enclave health");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "health should not panic with missing state: {stderr}"
    );
}

#[test]
fn doctor_is_idempotent() {
    let output1 = Command::new(common::enclave_bin())
        .args(["doctor"])
        .output()
        .expect("first doctor run");

    let output2 = Command::new(common::enclave_bin())
        .args(["doctor"])
        .output()
        .expect("second doctor run");

    assert_eq!(
        output1.status.code(),
        output2.status.code(),
        "doctor exit code should be stable across runs"
    );
}

#[test]
fn registry_repair_no_panic() {
    let output = Command::new(common::enclave_bin())
        .args(["registry", "repair"])
        .output()
        .expect("failed to run registry repair");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "registry repair should not panic: {stderr}"
    );
}
