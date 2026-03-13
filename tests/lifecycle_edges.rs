use std::process::Command;

mod common;

#[test]
fn start_nonexistent_sandbox_fails_gracefully() {
    let output = Command::new(common::enclave_bin())
        .args(["start", "nonexistent-sandbox-xyz"])
        .output()
        .expect("failed to run command");

    assert!(
        !output.status.success(),
        "starting a non-existent sandbox should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("not found") || combined.contains("error") || combined.contains("root"),
        "expected an error message, got stdout={stdout} stderr={stderr}"
    );
}

#[test]
fn stop_nonexistent_sandbox_fails_gracefully() {
    let output = Command::new(common::enclave_bin())
        .args(["stop", "nonexistent-sandbox-xyz"])
        .output()
        .expect("failed to run command");

    assert!(
        !output.status.success(),
        "stopping a non-existent sandbox should fail"
    );
}

#[test]
fn destroy_nonexistent_sandbox_fails_gracefully() {
    let output = Command::new(common::enclave_bin())
        .args(["destroy", "nonexistent-sandbox-xyz"])
        .output()
        .expect("failed to run command");

    assert!(
        !output.status.success(),
        "destroying a non-existent sandbox should fail"
    );
}

#[test]
fn sandbox_list_with_empty_state() {
    let output = Command::new(common::enclave_bin())
        .args(["list"])
        .output()
        .expect("failed to run command");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "list should not panic: {stderr}"
    );
}

#[test]
fn workspace_list_with_empty_state() {
    let output = Command::new(common::enclave_bin())
        .args(["workspace", "list"])
        .output()
        .expect("failed to run command");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "workspace list should not panic: {stderr}"
    );
}

#[test]
fn start_nonexistent_workspace_fails() {
    let output = Command::new(common::enclave_bin())
        .args([
            "workspace",
            "start",
            "nonexistent-sandbox",
            "nonexistent-workspace",
        ])
        .output()
        .expect("failed to run command");

    assert!(
        !output.status.success(),
        "starting a non-existent workspace should fail"
    );
}

#[test]
fn stop_nonexistent_workspace_fails() {
    let output = Command::new(common::enclave_bin())
        .args([
            "workspace",
            "stop",
            "nonexistent-sandbox",
            "nonexistent-workspace",
        ])
        .output()
        .expect("failed to run command");

    assert!(
        !output.status.success(),
        "stopping a non-existent workspace should fail"
    );
}

#[test]
fn destroy_nonexistent_workspace_fails() {
    let output = Command::new(common::enclave_bin())
        .args([
            "workspace",
            "destroy",
            "nonexistent-sandbox",
            "nonexistent-workspace",
        ])
        .output()
        .expect("failed to run command");

    assert!(
        !output.status.success(),
        "destroying a non-existent workspace should fail"
    );
}

#[test]
fn sandbox_status_subcommand_exists() {
    let output = Command::new(common::enclave_bin())
        .args(["status", "--help"])
        .output()
        .expect("failed to run command");

    assert!(output.status.success(), "status --help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("status") || stdout.contains("Usage"),
        "help should show status usage"
    );
}

#[test]
fn workspace_status_subcommand_exists() {
    let output = Command::new(common::enclave_bin())
        .args(["workspace", "status", "--help"])
        .output()
        .expect("failed to run command");

    assert!(
        output.status.success(),
        "workspace status --help should succeed"
    );
}

#[test]
fn sandbox_create_requires_root() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    let output = Command::new(common::enclave_bin())
        .args(["create", "test-no-root"])
        .output()
        .expect("failed to run command");

    assert!(!output.status.success(), "create should fail without root");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("root") || combined.contains("privilege") || combined.contains("sudo"),
        "error should mention root/privilege, got: {combined}"
    );
}
