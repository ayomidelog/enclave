use std::fs;
use std::process::Command;

mod common;

#[test]
fn top_level_help_includes_core_commands() {
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
    assert!(stdout.contains("workspace"));
    assert!(stdout.contains("snapshot"));
    assert!(stdout.contains("daemon"));
    assert!(stdout.contains("health"));
    assert!(stdout.contains("auth"));
    assert!(stdout.contains("stats"));
}

#[test]
fn top_level_help_includes_enclavefile_commands() {
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
    assert!(stdout.contains("init"), "help should list init command");
    assert!(stdout.contains("up"), "help should list up command");
    assert!(stdout.contains("down"), "help should list down command");
    assert!(
        stdout.contains("restart"),
        "help should list restart command"
    );
}

#[test]
fn up_help_shows_rebuild_flag() {
    let output = Command::new(common::enclave_bin())
        .args(["up", "--help"])
        .output()
        .expect("failed to run enclave up --help");
    assert!(
        output.status.success(),
        "command failed: {:?}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--rebuild"),
        "up --help should show --rebuild flag"
    );
}

#[test]
fn workspace_help_lists_enter_and_exec() {
    let output = Command::new(common::enclave_bin())
        .args(["workspace", "--help"])
        .output()
        .expect("failed to run enclave workspace --help");
    assert!(
        output.status.success(),
        "command failed: {:?}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("enter"));
    assert!(stdout.contains("exec"));
    assert!(stdout.contains("port"));
    assert!(stdout.contains("stats"));
}

#[test]
fn sandbox_create_help_lists_limit_flags() {
    let output = Command::new(common::enclave_bin())
        .args(["create", "--help"])
        .output()
        .expect("failed to run enclave create --help");
    assert!(
        output.status.success(),
        "command failed: {:?}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--cpu-percent"));
    assert!(stdout.contains("--memory-mb"));
    assert!(stdout.contains("--max-procs"));
}

#[test]
fn workspace_create_help_lists_limit_flags() {
    let output = Command::new(common::enclave_bin())
        .args(["workspace", "create", "--help"])
        .output()
        .expect("failed to run enclave workspace create --help");
    assert!(
        output.status.success(),
        "command failed: {:?}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--cpu-percent"));
    assert!(stdout.contains("--cpu-seconds"));
    assert!(stdout.contains("--memory-mb"));
}

#[test]
fn config_flag_is_accepted() {
    let temp_dir = std::env::temp_dir().join(format!("enclave-test-{}", std::process::id()));
    fs::create_dir_all(&temp_dir).expect("failed to create temp dir");
    let config_path = temp_dir.join("config.toml");
    fs::write(&config_path, "wait_secs = 7\n").expect("failed to write config");

    let output = Command::new(common::enclave_bin())
        .arg("--config")
        .arg(&config_path)
        .arg("--help")
        .output()
        .expect("failed to run enclave with --config");
    assert!(
        output.status.success(),
        "command failed: {:?}",
        output.status
    );
}
