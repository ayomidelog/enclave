use std::process::Command;

mod common;

fn run_enclave_command(args: &[&str]) -> std::process::Output {
    Command::new(common::enclave_bin())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run '{}': {e}", args.join(" ")))
}

fn assert_snapshot_subcommand_help_succeeds(subcommand: &str) {
    let output = run_enclave_command(&["snapshot", subcommand, "--help"]);
    assert!(
        output.status.success(),
        "snapshot {subcommand} --help should succeed: {:?}",
        output.status
    );
}

#[test]
fn snapshot_create_nonexistent_workspace_fails() {
    let output = run_enclave_command(&[
        "workspace",
        "snapshot",
        "no-such-sandbox",
        "no-such-workspace",
    ]);

    assert!(
        !output.status.success(),
        "snapshot create on non-existent workspace should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "should not panic: {stderr}");
}

#[test]
fn snapshot_list_nonexistent_workspace_fails() {
    let output = run_enclave_command(&[
        "workspace",
        "snapshot-list",
        "no-such-sandbox",
        "no-such-workspace",
    ]);

    assert!(
        !output.status.success(),
        "snapshot list on non-existent workspace should fail"
    );
}

#[test]
fn snapshot_gc_nonexistent_workspace_fails() {
    let output = run_enclave_command(&[
        "workspace",
        "snapshot-gc",
        "no-such-sandbox",
        "no-such-workspace",
    ]);

    assert!(
        !output.status.success(),
        "snapshot gc on non-existent workspace should fail"
    );
}

#[test]
fn snapshot_restore_nonexistent_fails() {
    let output = run_enclave_command(&[
        "workspace",
        "restore",
        "no-such-sandbox",
        "no-such-workspace",
        "no-such-snapshot",
    ]);

    assert!(
        !output.status.success(),
        "snapshot restore on non-existent workspace should fail"
    );
}

#[test]
fn snapshot_help_is_available() {
    let output = run_enclave_command(&["workspace", "snapshot", "--help"]);

    assert!(
        output.status.success(),
        "workspace snapshot --help should succeed: {:?}",
        output.status
    );
}

#[test]
fn top_level_snapshot_help_is_available() {
    let output = run_enclave_command(&["snapshot", "--help"]);

    assert!(
        output.status.success(),
        "snapshot --help should succeed: {:?}",
        output.status
    );
}

#[test]
fn top_level_snapshot_create_help_is_available() {
    assert_snapshot_subcommand_help_succeeds("create");
}

#[test]
fn top_level_snapshot_list_help_is_available() {
    assert_snapshot_subcommand_help_succeeds("list");
}

#[test]
fn top_level_snapshot_restore_help_is_available() {
    assert_snapshot_subcommand_help_succeeds("restore");
}

#[test]
fn top_level_snapshot_export_help_is_available() {
    assert_snapshot_subcommand_help_succeeds("export");
}

#[test]
fn top_level_snapshot_import_help_is_available() {
    assert_snapshot_subcommand_help_succeeds("import");
}

#[test]
fn snapshot_list_help_is_available() {
    let output = run_enclave_command(&["workspace", "snapshot-list", "--help"]);

    assert!(
        output.status.success(),
        "workspace snapshot-list --help should succeed: {:?}",
        output.status
    );
}

#[test]
fn snapshot_gc_help_mentions_keep() {
    let output = run_enclave_command(&["workspace", "snapshot-gc", "--help"]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("keep"),
        "snapshot-gc help should mention keep: {stdout}"
    );
}

#[test]
fn restore_help_is_available() {
    let output = run_enclave_command(&["workspace", "restore", "--help"]);

    assert!(
        output.status.success(),
        "workspace restore --help should succeed: {:?}",
        output.status
    );
}
