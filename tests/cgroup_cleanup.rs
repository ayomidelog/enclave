//! The cgroup surface of `doctor`, and the help of the commands around it.
//!
//! `doctor` needs root to read the cgroup hierarchy, so on an unprivileged runner it
//! is answered by the root gate. The help assertions do not need root and run either
//! way, which is what makes them the same test on both runners.

mod common;

use common::{combined_of, run_cli, CliRun};

/// The doctor report includes a cgroup check, named so an operator can find it.
#[test]
fn doctor_checks_stale_cgroups() {
    match run_cli(&["doctor"]) {
        CliRun::RefusedForRoot(output) => assert!(
            combined_of(&output).contains("require root privileges"),
            "the gate must say why it refused: {}",
            combined_of(&output)
        ),
        CliRun::Ran(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains("cgroup"),
                "the report must include a cgroup check: {stdout}"
            );
        }
    }
}

/// `health` reports the cgroup state it can see, or says it cannot see it.
#[test]
fn health_reports_cgroup_status() {
    match run_cli(&["health"]) {
        CliRun::RefusedForRoot(_) => {}
        CliRun::Ran(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = common::stderr_of(&output);
            assert!(
                !stderr.contains("panicked"),
                "health should not panic: {stderr}"
            );
            // A host with cgroup v2 reports it; one without says the capability is
            // missing rather than silently omitting it.
            assert!(
                stdout.contains("cgroup"),
                "health must say something about cgroups: {stdout}"
            );
        }
    }
}

/// The doctor report names each check, so a cgroup problem is attributable.
#[test]
fn doctor_output_includes_named_checks() {
    match run_cli(&["doctor"]) {
        CliRun::RefusedForRoot(_) => {}
        CliRun::Ran(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains("\"name\"") && stdout.contains("\"status\""),
                "each check must carry a name and a status: {stdout}"
            );
        }
    }
}

/// The doctor help is available without root.
#[test]
fn doctor_help_is_available() {
    match run_cli(&["doctor", "--help"]) {
        CliRun::RefusedForRoot(output) => {
            panic!("help must not need root: {}", common::combined_of(&output))
        }
        CliRun::Ran(output) => assert!(
            output.status.success(),
            "doctor --help should succeed: {:?}",
            output.status
        ),
    }
}

/// The health help is available without root.
#[test]
fn health_help_is_available() {
    match run_cli(&["health", "--help"]) {
        CliRun::RefusedForRoot(output) => {
            panic!("help must not need root: {}", common::combined_of(&output))
        }
        CliRun::Ran(output) => assert!(
            output.status.success(),
            "health --help should succeed: {:?}",
            output.status
        ),
    }
}
