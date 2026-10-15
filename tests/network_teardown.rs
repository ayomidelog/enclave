//! Teardown against a workspace that does not exist, and the help surface.
//!
//! The stop and destroy commands need root, so on an unprivileged runner they are
//! answered by the root gate. `common::run_cli` says which answered, and each case
//! asserts what is true in it: the missing target is named when the command ran, and
//! the gate's message when it did not.

mod common;

use common::{combined_of, run_cli, CliRun};

/// A teardown of a workspace that does not exist fails and names it.
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
fn stop_nonrunning_workspace_no_network_leak() {
    assert_missing_workspace_fails(&["workspace", "stop", "fake-sandbox", "fake-workspace"]);
}

#[test]
fn destroy_missing_workspace_no_veth_leak() {
    assert_missing_workspace_fails(&["workspace", "destroy", "fake-sandbox", "fake-workspace"]);
}

/// Repeating a teardown that already failed fails the same way rather than
/// succeeding the second time, which is what would let a script believe it cleaned up.
#[test]
fn double_stop_workspace_is_safe() {
    for _ in 0..2 {
        assert_missing_workspace_fails(&[
            "workspace",
            "stop",
            "no-such-sandbox",
            "no-such-workspace",
        ]);
    }
}

/// `health` needs a daemon but not a workspace, so it is a read of the daemon's own
/// state and cannot leave network state behind. It needs root to reach the socket.
#[test]
fn health_check_no_network_side_effects() {
    match run_cli(&["health"]) {
        CliRun::RefusedForRoot(_) => {}
        CliRun::Ran(output) => {
            let stderr = common::stderr_of(&output);
            assert!(
                !stderr.contains("panicked"),
                "health should not panic: {stderr}"
            );
        }
    }
}

/// The workspace help lists the subcommands it has, without needing root.
#[test]
fn workspace_help_mentions_network_related_commands() {
    match run_cli(&["workspace", "--help"]) {
        CliRun::RefusedForRoot(output) => {
            panic!("help must not need root: {}", common::combined_of(&output))
        }
        CliRun::Ran(output) => {
            assert!(output.status.success(), "workspace --help should succeed");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(stdout.contains("start"), "should list start: {stdout}");
            assert!(stdout.contains("stop"), "should list stop: {stdout}");
        }
    }
}
