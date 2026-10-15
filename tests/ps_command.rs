//! The `ps` command: its help surface, its flags, and its root requirement.
//!
//! `ps` needs root to read the daemon's process list, so the parts that run it are
//! answered by the root gate on an unprivileged runner. The help and flag-parsing
//! assertions do not need root, and they are the ones that pin the interface.

mod common;

use common::{combined_of, run_cli, CliRun};

/// The help shows the command and both of its flags, without needing root.
#[test]
fn ps_help_is_available() {
    match run_cli(&["ps", "--help"]) {
        CliRun::RefusedForRoot(output) => {
            panic!("help must not need root: {}", common::combined_of(&output))
        }
        CliRun::Ran(output) => {
            assert!(
                output.status.success(),
                "ps --help should succeed: {:?}",
                output.status
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(stdout.contains("Usage"), "help should show usage: {stdout}");
            assert!(
                stdout.contains("--local") && stdout.contains("--project"),
                "help should include the local/project flags: {stdout}"
            );
        }
    }
}

/// `--project` is accepted by the parser, so it reaches the command rather than being
/// refused as an unknown argument.
#[test]
fn ps_project_alias_is_accepted_by_the_parser() {
    let output = std::process::Command::new(common::enclave_bin())
        .args(["ps", "--project"])
        .output()
        .expect("failed to run enclave ps --project");
    let message = combined_of(&output);
    assert!(
        !message.contains("unexpected argument '--project'"),
        "--project must be a known flag: {message}"
    );
    assert!(!message.contains("panicked"), "must not panic: {message}");
}

/// `ps` is listed in the top-level help, which is how it is discovered.
#[test]
fn ps_is_listed_in_top_level_help() {
    match run_cli(&["--help"]) {
        CliRun::RefusedForRoot(output) => {
            panic!("help must not need root: {}", common::combined_of(&output))
        }
        CliRun::Ran(output) => {
            assert!(output.status.success(), "--help should succeed");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains("ps"),
                "top-level help should list ps: {stdout}"
            );
        }
    }
}

/// Running `ps` needs root, and the refusal says so.
///
/// This test's subject is the gate, so it is the one that asserts on the refusal. It
/// is skipped when the suite runs as root, where there is no gate to observe.
#[test]
fn ps_requires_root() {
    if common::is_root() {
        return;
    }
    let output = std::process::Command::new(common::enclave_bin())
        .args(["ps"])
        .output()
        .expect("failed to run enclave ps");

    assert!(!output.status.success(), "ps should fail without root");
    let message = combined_of(&output);
    assert!(
        message.contains("root privileges") || message.contains("sudo"),
        "the error should say what is missing: {message}"
    );
}

/// Repeating `ps` is stable, and never panics.
#[test]
fn ps_sequential_calls_are_stable() {
    for iteration in 0..3 {
        match run_cli(&["ps"]) {
            CliRun::RefusedForRoot(_) => return,
            CliRun::Ran(output) => {
                let stderr = common::stderr_of(&output);
                assert!(
                    !stderr.contains("panicked"),
                    "iteration {iteration}: ps must not panic: {stderr}"
                );
            }
        }
    }
}
