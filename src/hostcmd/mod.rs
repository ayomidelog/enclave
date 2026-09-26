//! Bounded execution of host commands.
//!
//! Enclave drives its host work through external programs: `ip`, `mount`,
//! `umount`, `iptables`, `losetup`, `mkfs.ext4`, `resize2fs`, and `e2fsck`. A
//! command that never returns would otherwise hold a daemon worker, and with it
//! a registry lock or a lifecycle lease, for as long as the process lives.
//!
//! Every call through this module therefore has three properties:
//!
//! - a deadline, so no command can outlive its budget;
//! - capped output, so a chatty command cannot exhaust daemon memory;
//! - a structured failure, so a caller can tell a timeout from a non-zero exit
//!   without matching on message text.
//!
//! Commands that are expected to be long lived (`debootstrap`, the session
//! helper, the workspace runtime) do not belong here; they have their own
//! supervision.
//!
//! The builder is in this file, because describing a command is one subject and
//! running it is another. The pieces both depend on are split out too: `error`
//! for the failure and output types, `run` for the spawn and its deadline,
//! `namespace` for entering another process network namespace, `lookup` for
//! finding an executable and reading the configured deadline, and `capture` for
//! draining and capping the pipes.

mod capture;
mod error;
mod lookup;
mod namespace;
mod run;

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};

use capture::{wait_until_deadline, OUTPUT_GRACE};

pub(crate) use error::{HostCommandError, HostCommandFailure, HostOutput};
pub(crate) use lookup::{command_on_path, configured_timeout, is_timeout};
use namespace::enter_network_namespace;

/// Default deadline for a host command.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Hard upper bound on a configured deadline. A command allowed to run longer
/// than this should be supervised rather than awaited.
pub(crate) const MAX_TIMEOUT: Duration = Duration::from_secs(3600);

/// Default cap on each of stdout and stderr.
pub(crate) const DEFAULT_OUTPUT_CAP: usize = 1024 * 1024;

#[derive(Debug)]
pub(crate) struct HostCommand {
    pub(super) program: OsString,
    pub(super) args: Vec<OsString>,
    pub(super) stdin: Option<Vec<u8>>,
    pub(super) timeout: Duration,
    pub(super) output_cap: usize,
    pub(super) capture_output: bool,
    /// Network namespace the child joins before exec, given as a pid.
    ///
    /// nsenter exists to do exactly this, and it costs a process on the workspace
    /// start path. Joining the namespace in the child's own pre-exec hook does the
    /// same thing with no extra process, and keeps the deadline and output
    /// handling identical to every other host command.
    pub(super) netns_pid: Option<u32>,
}

impl HostCommand {
    pub(crate) fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            stdin: None,
            timeout: configured_timeout(),
            output_cap: DEFAULT_OUTPUT_CAP,
            capture_output: true,
            netns_pid: None,
        }
    }

    /// Run the command inside the network namespace of this pid.
    pub(crate) fn netns(mut self, pid: u32) -> Self {
        self.netns_pid = Some(pid);
        self
    }

    /// Discard stdout and stderr instead of capturing them.
    ///
    /// Probe and check commands run on the workspace start path and their output
    /// is never read. Skipping the pipes keeps those calls to one spawn.
    pub(crate) fn discard_output(mut self) -> Self {
        self.capture_output = false;
        self
    }

    pub(crate) fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub(crate) fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_os_string()));
        self
    }

    /// Feed `bytes` to the command standard input.
    pub(crate) fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self
    }

    /// Override the deadline, clamped to MAX_TIMEOUT.
    pub(crate) fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.min(MAX_TIMEOUT);
        self
    }

    pub(crate) fn output_cap(mut self, cap: usize) -> Self {
        self.output_cap = cap;
        self
    }

    /// The command line, for error reports and tracing.
    pub(crate) fn describe(&self) -> String {
        let program = self.program.to_string_lossy();
        if self.args.is_empty() {
            return program.into_owned();
        }
        let args = self
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        format!("{program} {args}")
    }
}

#[cfg(test)]
#[path = "../../tests/src/hostcmd.rs"]
mod tests;
