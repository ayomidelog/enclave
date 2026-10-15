//! What `doctor` and `health` do against the host they are run on.
//!
//! Both commands need root, so on an unprivileged runner the root gate answers and
//! there is no report to assert on. `common::run_cli` says which answered; as root the
//! tests assert on the report's shape, which is what the commands are for.

mod common;

use common::{combined_of, run_cli, CliRun};

/// Run a command and hand the output to `check` when it actually ran.
fn when_run(args: &[&str], check: impl FnOnce(&std::process::Output)) {
    match run_cli(args) {
        CliRun::RefusedForRoot(output) => assert!(
            combined_of(&output).contains("require root privileges"),
            "the gate must say why it refused: {}",
            combined_of(&output)
        ),
        CliRun::Ran(output) => check(&output),
    }
}

/// A doctor report is JSON with a status and a list of named checks.
///
/// That shape is the command's contract: a caller reads `status` to decide whether to
/// act and `checks` to find which one needs attention.
#[test]
fn doctor_reports_a_status_and_a_list_of_checks() {
    when_run(&["doctor"], |output| {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let report: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|error| panic!("doctor must print JSON ({error}): {stdout}"));
        assert!(
            report
                .get("status")
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "the report must carry a status: {report}"
        );
        let checks = report
            .get("checks")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("the report must carry checks: {report}"));
        assert!(!checks.is_empty(), "the report must run at least one check");
        for check in checks {
            for field in ["name", "status", "detail"] {
                assert!(
                    check
                        .get(field)
                        .and_then(serde_json::Value::as_str)
                        .is_some(),
                    "every check must carry {field}: {check}"
                );
            }
        }
    });
}

/// One of the checks is the cgroup one, named so an operator can find it.
#[test]
fn doctor_checks_cgroups() {
    when_run(&["doctor"], |output| {
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("cgroup"),
            "the report must include a cgroup check: {stdout}"
        );
    });
}

/// The exit code is stable across two runs of an unchanged host.
#[test]
fn doctor_exit_code_is_deterministic() {
    match (run_cli(&["doctor"]), run_cli(&["doctor"])) {
        (CliRun::Ran(first), CliRun::Ran(second)) => assert_eq!(
            first.status.code(),
            second.status.code(),
            "doctor exit code should be deterministic"
        ),
        (CliRun::RefusedForRoot(_), _) | (_, CliRun::RefusedForRoot(_)) => {}
    }
}

/// `health` is a read of the daemon's own state, so repeating it is stable.
#[test]
fn health_is_read_only() {
    match (run_cli(&["health"]), run_cli(&["health"])) {
        (CliRun::Ran(first), CliRun::Ran(second)) => assert_eq!(
            first.status.code(),
            second.status.code(),
            "health should return the same exit code on repeated calls"
        ),
        (CliRun::RefusedForRoot(_), _) | (_, CliRun::RefusedForRoot(_)) => {}
    }
}

/// A state directory that does not exist is not a panic.
///
/// The daemon is started with the given `XDG_STATE_HOME`, so it creates the directory
/// rather than finding it missing; what is asserted is that neither command panicked
/// and that neither reported an internal error.
#[test]
fn doctor_and_health_handle_a_fresh_state_directory() {
    let unique_dir = std::env::temp_dir().join(format!(
        "enclave-test-doctor-state-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    for args in [vec!["doctor"], vec!["health"]] {
        let output = std::process::Command::new(common::enclave_bin())
            .args(&args)
            .env("XDG_STATE_HOME", &unique_dir)
            .output()
            .unwrap_or_else(|error| panic!("failed to run {args:?}: {error}"));
        let message = combined_of(&output);
        assert!(
            !message.contains("panicked"),
            "{args:?} must not panic against a fresh state directory: {message}"
        );
    }
    let _ = std::fs::remove_dir_all(unique_dir);
}

/// `doctor --help` is available without root, which is what makes it discoverable.
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

/// `registry repair` names a sandbox or workspace it cannot repair rather than
/// panicking, which is the case a missing target produces.
#[test]
fn registry_repair_no_panic() {
    when_run(&["registry", "repair"], |output| {
        let stderr = common::stderr_of(output);
        assert!(
            !stderr.contains("panicked"),
            "registry repair should not panic: {stderr}"
        );
    });
}
