use std::process::Command;

mod common;

fn assert_command_in_help(command_name: &str) {
    let output = Command::new(common::enclave_bin())
        .arg("--help")
        .output()
        .expect("failed to run enclave --help");
    assert!(
        output.status.success(),
        "command failed: {:?}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(command_name),
        "help output should list the {} command",
        command_name
    );
}

#[test]
fn doctor_command_is_listed_in_help() {
    assert_command_in_help("doctor");
}

#[test]
fn health_command_is_listed_in_help() {
    assert_command_in_help("health");
}

#[test]
fn version_output_contains_release_version() {
    let output = Command::new(common::enclave_bin())
        .arg("--version")
        .output()
        .expect("failed to run enclave --version");
    assert!(
        output.status.success(),
        "command failed: {:?}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "enclave 1.0.2");
}
