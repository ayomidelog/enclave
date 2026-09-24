//! How a host command failed, and what it produced.

use std::process::ExitStatus;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostCommandFailure {
    /// The program could not be started at all.
    Spawn,
    /// The deadline expired and the command was stopped.
    TimedOut,
    /// The command ran to completion with a non-zero status.
    ExitStatus,
}

/// A failed host command, carrying the detail a caller needs to report it.
#[derive(Debug)]
pub(crate) struct HostCommandError {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) kind: HostCommandFailure,
    pub(crate) status: Option<i32>,
    pub(crate) stderr: String,
    pub(crate) timeout: Duration,
}

impl std::fmt::Display for HostCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let command = self.describe();
        match self.kind {
            HostCommandFailure::Spawn => write!(formatter, "failed to run: {command}"),
            HostCommandFailure::TimedOut => write!(
                formatter,
                "{command} did not finish within {:?} and was stopped",
                self.timeout
            ),
            HostCommandFailure::ExitStatus => {
                let status = self
                    .status
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".to_string());
                if self.stderr.is_empty() {
                    write!(formatter, "{command} failed (status {status})")
                } else {
                    write!(
                        formatter,
                        "{command} failed (status {status}): {}",
                        self.stderr
                    )
                }
            }
        }
    }
}

impl std::error::Error for HostCommandError {}

impl HostCommandError {
    /// The command line, for a report that names what was run.
    pub(crate) fn describe(&self) -> String {
        if self.args.is_empty() {
            return self.program.clone();
        }
        format!("{} {}", self.program, self.args.join(" "))
    }

    /// True when the deadline expired rather than the command failing.
    pub(crate) fn is_timeout(&self) -> bool {
        self.kind == HostCommandFailure::TimedOut
    }
}

/// The result of a host command that completed within its deadline.
#[derive(Debug)]
pub(crate) struct HostOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

impl HostOutput {
    pub(crate) fn success(&self) -> bool {
        self.status.success()
    }

    /// stdout as text, lossily decoded because command output is diagnostic.
    pub(crate) fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// stderr as text, trimmed of the trailing newline commands usually add.
    pub(crate) fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}
