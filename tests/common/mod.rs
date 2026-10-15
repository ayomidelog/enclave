//! Shared helpers for the CLI-surface tests.
//!
//! These tests run the real binary and assert on what it printed. Most of the
//! commands they exercise need root, and the CLI refuses to run as a non-root user
//! before it does anything else — so on an unprivileged runner every one of them
//! exits at that gate, and an assertion that only checks "it failed" or "it did not
//! panic" is satisfied by the refusal rather than by the behaviour the test is named
//! for. [`run_cli`] makes the distinction explicit so a test can say what it expects
//! in each case.

// This module is compiled into every CLI-surface test target, and each one uses a
// different subset of it, so an unused helper here is expected rather than dead.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Command, Output};

pub fn enclave_bin() -> PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_enclave") {
        return PathBuf::from(path);
    }

    let exe = std::env::current_exe().expect("failed to resolve current test executable path");
    let debug_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("failed to resolve target debug directory");
    debug_dir.join("enclave")
}

/// What the CLI did with one invocation.
pub enum CliRun {
    /// The root gate answered: the command never ran. This is what every
    /// host-touching command does on an unprivileged runner.
    RefusedForRoot(Output),
    /// The command ran and this is what it produced.
    Ran(Output),
}

/// Whether this process can run commands that touch host state.
pub fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Run the CLI and say whether the root gate or the command itself answered.
///
/// The gate's message is checked rather than assumed, so a change to it fails here
/// loudly instead of making every caller's `RefusedForRoot` arm silently wrong.
pub fn run_cli(args: &[&str]) -> CliRun {
    let output = Command::new(enclave_bin())
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("failed to run enclave {}: {error}", args.join(" ")));
    if !is_root() && is_root_refusal(&output) {
        return CliRun::RefusedForRoot(output);
    }
    CliRun::Ran(output)
}

/// Whether this output is the CLI's refusal to run without root.
fn is_root_refusal(output: &Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    !output.status.success() && stderr.contains("require root privileges")
}

/// The stderr of an output, for a message assertion.
pub fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The combined stdout and stderr of an output, for a message assertion.
pub fn combined_of(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
