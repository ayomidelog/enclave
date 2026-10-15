//! What the CLI does with a target that does not exist, and with an empty state.
//!
//! These commands need root, so on an unprivileged runner they are answered by the
//! root gate rather than by the lifecycle code. `common::run_cli` says which of the
//! two answered, and each test asserts the thing that is true in the case it is in:
//! the gate's message when the command did not run, and the command's own error when
//! it did. An assertion that held in both cases would pass without either behaviour
//! working, which is what these used to do.

mod common;

use common::{combined_of, run_cli, CliRun};

/// A command that names a target which does not exist fails, and says so.
///
/// `expected` is the part of the message that is about the missing target rather than
/// about the privilege the caller lacked.
fn assert_missing_target_fails(args: &[&str], expected: &str) {
    match run_cli(args) {
        CliRun::RefusedForRoot(output) => {
            assert!(
                combined_of(&output).contains("require root privileges"),
                "the gate must say why it refused: {}",
                combined_of(&output)
            );
        }
        CliRun::Ran(output) => {
            let message = combined_of(&output);
            assert!(
                !output.status.success(),
                "{args:?} against a missing target must fail: {message}"
            );
            assert!(
                message.contains(expected),
                "{args:?} must name the missing target ({expected:?}): {message}"
            );
        }
    }
}

#[test]
fn start_nonexistent_sandbox_fails_gracefully() {
    assert_missing_target_fails(&["start", "nonexistent-sandbox-xyz"], "not found");
}

#[test]
fn stop_nonexistent_sandbox_fails_gracefully() {
    assert_missing_target_fails(&["stop", "nonexistent-sandbox-xyz"], "not found");
}

#[test]
fn destroy_nonexistent_sandbox_fails_gracefully() {
    assert_missing_target_fails(&["destroy", "nonexistent-sandbox-xyz"], "not found");
}

#[test]
fn start_nonexistent_workspace_fails() {
    assert_missing_target_fails(
        &[
            "workspace",
            "start",
            "nonexistent-sandbox",
            "nonexistent-workspace",
        ],
        "not found",
    );
}

#[test]
fn stop_nonexistent_workspace_fails() {
    assert_missing_target_fails(
        &[
            "workspace",
            "stop",
            "nonexistent-sandbox",
            "nonexistent-workspace",
        ],
        "not found",
    );
}

#[test]
fn destroy_nonexistent_workspace_fails() {
    assert_missing_target_fails(
        &[
            "workspace",
            "destroy",
            "nonexistent-sandbox",
            "nonexistent-workspace",
        ],
        "not found",
    );
}

/// A listing against an empty state is an answer, not a failure.
///
/// The two listings start a daemon when one is not running, so as root they succeed
/// against whatever state the host has; the assertion is that neither panicked.
fn assert_listing_is_an_answer(args: &[&str]) {
    match run_cli(args) {
        CliRun::RefusedForRoot(_) => {}
        CliRun::Ran(output) => {
            let stderr = common::stderr_of(&output);
            assert!(
                !stderr.contains("panicked"),
                "{args:?} must not panic: {stderr}"
            );
        }
    }
}

#[test]
fn sandbox_list_with_empty_state() {
    assert_listing_is_an_answer(&["list"]);
}

#[test]
fn workspace_list_with_empty_state() {
    assert_listing_is_an_answer(&["workspace", "list"]);
}

/// The two status subcommands exist and their help is available without root.
#[test]
fn status_subcommands_have_help() {
    for args in [
        vec!["status", "--help"],
        vec!["workspace", "status", "--help"],
    ] {
        match run_cli(&args) {
            CliRun::RefusedForRoot(output) => {
                panic!("help must not need root: {}", common::combined_of(&output))
            }
            CliRun::Ran(output) => {
                assert!(output.status.success(), "{args:?} --help should succeed");
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(
                    stdout.contains("Usage") || stdout.contains("status"),
                    "{args:?} --help should show usage: {stdout}"
                );
            }
        }
    }
}

/// A command that touches host state is refused when the caller is not root.
///
/// This is the one test in the file whose subject *is* the gate, so it is the only
/// one that asserts on the refusal rather than working around it. It is skipped when
/// the suite runs as root, where there is no gate to observe.
#[test]
fn sandbox_create_requires_root() {
    if common::is_root() {
        return;
    }
    let output = std::process::Command::new(common::enclave_bin())
        .args(["create", "test-no-root"])
        .output()
        .expect("failed to run command");

    assert!(!output.status.success(), "create should fail without root");
    let message = combined_of(&output);
    assert!(
        message.contains("root privileges") || message.contains("sudo"),
        "the error should say what is missing, got: {message}"
    );
}
