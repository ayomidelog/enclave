//! Repeated and concurrent commands must not contend on the state locks.
//!
//! The registry and policy locks are held only for the mutations themselves, never
//! across the host work a lifecycle operation does, so a plain read that has to wait
//! for one is a sign a holder is doing something inside the lock. These tests drive the
//! read-only commands and require that none of them reports a lock wait.
//!
//! The commands need root, so on an unprivileged runner they are answered by the root
//! gate before any lock is taken; the tests then have nothing to observe and say so
//! rather than asserting on a command that never ran.

mod common;

use common::{run_cli, CliRun};

/// Run a read-only command `iterations` times and require no lock contention.
fn assert_no_lock_contention(args: &[&str], iterations: usize) {
    for iteration in 0..iterations {
        match run_cli(args) {
            CliRun::RefusedForRoot(_) => return,
            CliRun::Ran(output) => {
                let stderr = common::stderr_of(&output);
                assert!(
                    !stderr.contains("panicked"),
                    "iteration {iteration}: {args:?} must not panic: {stderr}"
                );
                assert!(
                    !stderr.contains("failed to lock"),
                    "iteration {iteration}: {args:?} must not contend on a lock: {stderr}"
                );
            }
        }
    }
}

#[test]
fn sequential_sandbox_list_no_lock_contention() {
    assert_no_lock_contention(&["list"], 3);
}

#[test]
fn sequential_workspace_list_no_lock_contention() {
    assert_no_lock_contention(&["workspace", "list"], 3);
}

#[test]
fn rapid_doctor_calls_no_lock_issue() {
    assert_no_lock_contention(&["doctor"], 3);
}

#[test]
fn rapid_health_calls_no_lock_issue() {
    assert_no_lock_contention(&["health"], 3);
}

/// Several reads at once must not make any of them wait for a lock.
fn assert_concurrent_reads_are_safe(args: &[&str]) {
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let owned = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
            std::thread::spawn(move || {
                let borrowed = owned.iter().map(String::as_str).collect::<Vec<_>>();
                common::run_cli(&borrowed)
            })
        })
        .collect();

    for (index, handle) in handles.into_iter().enumerate() {
        let run = handle
            .join()
            .unwrap_or_else(|error| panic!("thread {index}: {error:?}"));
        if let CliRun::Ran(output) = run {
            let stderr = common::stderr_of(&output);
            assert!(
                !stderr.contains("panicked"),
                "thread {index}: {args:?} must not panic: {stderr}"
            );
            assert!(
                !stderr.contains("failed to lock"),
                "thread {index}: {args:?} must not contend: {stderr}"
            );
        }
    }
}

#[test]
fn concurrent_sandbox_list_is_safe() {
    assert_concurrent_reads_are_safe(&["list"]);
}

#[test]
fn concurrent_doctor_is_safe() {
    assert_concurrent_reads_are_safe(&["doctor"]);
}

/// The repair help is available without root, which is what makes it discoverable.
#[test]
fn registry_repair_help_available() {
    match run_cli(&["registry", "repair", "--help"]) {
        CliRun::RefusedForRoot(output) => {
            panic!("help must not need root: {}", common::combined_of(&output))
        }
        CliRun::Ran(output) => assert!(
            output.status.success(),
            "registry repair --help should succeed: {:?}",
            output.status
        ),
    }
}
