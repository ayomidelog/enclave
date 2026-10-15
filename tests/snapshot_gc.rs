//! The snapshot command surface: naming a workspace that does not exist, and help.
//!
//! The snapshot commands need root, so on an unprivileged runner they are answered by
//! the root gate. The help assertions do not, and they are what pin the subcommand
//! tree.

mod common;

use common::{combined_of, run_cli, CliRun};

/// A snapshot command naming a workspace that does not exist fails and names it.
fn assert_missing_workspace_fails(args: &[&str]) {
    match run_cli(args) {
        CliRun::RefusedForRoot(output) => assert!(
            combined_of(&output).contains("require root privileges"),
            "the gate must say why it refused: {}",
            combined_of(&output)
        ),
        CliRun::Ran(output) => {
            let message = combined_of(&output);
            assert!(
                !output.status.success(),
                "{args:?} must fail against a missing workspace: {message}"
            );
            assert!(
                message.contains("not found"),
                "{args:?} must name the missing workspace: {message}"
            );
            assert!(!message.contains("panicked"), "must not panic: {message}");
        }
    }
}

#[test]
fn snapshot_create_nonexistent_workspace_fails() {
    assert_missing_workspace_fails(&[
        "workspace",
        "snapshot",
        "no-such-sandbox",
        "no-such-workspace",
    ]);
}

#[test]
fn snapshot_list_nonexistent_workspace_fails() {
    assert_missing_workspace_fails(&[
        "workspace",
        "snapshot-list",
        "no-such-sandbox",
        "no-such-workspace",
    ]);
}

#[test]
fn snapshot_gc_nonexistent_workspace_fails() {
    assert_missing_workspace_fails(&[
        "workspace",
        "snapshot-gc",
        "no-such-sandbox",
        "no-such-workspace",
    ]);
}

#[test]
fn snapshot_restore_nonexistent_fails() {
    assert_missing_workspace_fails(&[
        "workspace",
        "restore",
        "no-such-sandbox",
        "no-such-workspace",
        "no-such-snapshot",
    ]);
}

/// Help is available without root, and every subcommand has it.
fn assert_help_succeeds(args: &[&str]) {
    match run_cli(args) {
        CliRun::RefusedForRoot(output) => {
            panic!("help must not need root: {}", common::combined_of(&output))
        }
        CliRun::Ran(output) => assert!(
            output.status.success(),
            "{args:?} should succeed: {:?}",
            output.status
        ),
    }
}

#[test]
fn snapshot_help_is_available() {
    assert_help_succeeds(&["workspace", "snapshot", "--help"]);
    assert_help_succeeds(&["snapshot", "--help"]);
}

#[test]
fn every_snapshot_subcommand_has_help() {
    for subcommand in ["create", "list", "restore", "export", "import"] {
        assert_help_succeeds(&["snapshot", subcommand, "--help"]);
    }
}

#[test]
fn snapshot_list_help_is_available() {
    assert_help_succeeds(&["workspace", "snapshot-list", "--help"]);
}

/// The gc help names the flag that controls retention, which is the one that matters.
#[test]
fn snapshot_gc_help_mentions_keep() {
    match run_cli(&["workspace", "snapshot-gc", "--help"]) {
        CliRun::RefusedForRoot(output) => {
            panic!("help must not need root: {}", common::combined_of(&output))
        }
        CliRun::Ran(output) => {
            assert!(output.status.success(), "snapshot-gc --help should succeed");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains("keep"),
                "snapshot-gc help should mention keep: {stdout}"
            );
        }
    }
}

#[test]
fn restore_help_is_available() {
    assert_help_succeeds(&["workspace", "restore", "--help"]);
}
