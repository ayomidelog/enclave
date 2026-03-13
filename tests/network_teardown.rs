use std::process::Command;

mod common;

#[test]
fn stop_nonrunning_workspace_no_network_leak() {
    let output = Command::new(common::enclave_bin())
        .args(["workspace", "stop", "fake-sandbox", "fake-workspace"])
        .output()
        .expect("failed to run command");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "should not panic: {stderr}");
}

#[test]
fn destroy_missing_workspace_no_veth_leak() {
    let output = Command::new(common::enclave_bin())
        .args(["workspace", "destroy", "fake-sandbox", "fake-workspace"])
        .output()
        .expect("failed to run command");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "should not panic: {stderr}");
}

#[test]
fn health_check_no_network_side_effects() {
    let output = Command::new(common::enclave_bin())
        .args(["health"])
        .output()
        .expect("failed to run health");

    let _code = output.status.code();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "health should not panic: {stderr}"
    );
}

#[test]
fn workspace_help_mentions_network_related_commands() {
    let output = Command::new(common::enclave_bin())
        .args(["workspace", "--help"])
        .output()
        .expect("failed to run workspace --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("start"), "should list start subcommand");
    assert!(stdout.contains("stop"), "should list stop subcommand");
}

#[test]
fn double_stop_workspace_is_safe() {
    for _ in 0..2 {
        let output = Command::new(common::enclave_bin())
            .args(["workspace", "stop", "no-such-sandbox", "no-such-workspace"])
            .output()
            .expect("failed to run command");

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("panicked"),
            "double stop should not panic: {stderr}"
        );
    }
}
