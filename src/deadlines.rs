//! The deadlines the lifecycle waits on, and the operator overrides for them.
//!
//! Every wait in the lifecycle is bounded, and each bound is a trade. Too short
//! and a slow host fails a start that would have finished; too long and a broken
//! runtime holds a worker, a lifecycle lease, and a registry lock for the whole
//! wait. The defaults suit a local host. An operator whose storage or CPU is
//! slower than that can raise one through the environment, but never past a hard
//! ceiling, because a deadline with no ceiling is not a deadline.
//!
//! A value is read when it is used, so an override takes effect on the next
//! operation without a restart, and the whole table is reported in daemon health
//! so an operator can see which values are actually in force.
//!
//! Deadlines that belong to a long-running job with its own supervision (a
//! debootstrap, a rootfs copy, a snapshot archive) are not here. They are
//! bounded where that job is defined, and they are minutes rather than seconds.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::hostcmd::{DEFAULT_TIMEOUT as HOST_COMMAND_DEFAULT, MAX_TIMEOUT as HOST_COMMAND_MAX};

/// Variables already reported as unusable.
///
/// A deadline is resolved when it is used, and the host-command deadline is
/// resolved for every host command, so an unusable value would otherwise warn on
/// each of the dozens of commands one start runs. The first warning names the
/// mistake; the rest would only bury it.
static WARNED: OnceLock<Mutex<Vec<&'static str>>> = OnceLock::new();

fn warn_once(variable: &'static str, raw: &str, default: Duration) {
    let warned = WARNED.get_or_init(|| Mutex::new(Vec::new()));
    let mut warned = warned.lock().unwrap_or_else(|error| error.into_inner());
    if warned.contains(&variable) {
        return;
    }
    warned.push(variable);
    tracing::warn!("ignoring {variable} value '{raw}'; using {default:?}");
}

/// The unit an environment variable is written in.
///
/// A sub-second wait has to be expressible, so the lifecycle waits use
/// milliseconds. The host-command deadline predates this table and is published
/// in seconds, and its variable name is part of the interface an operator
/// already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Milliseconds,
    Seconds,
}

/// One bounded wait, with the value in force and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Deadline {
    /// A short label for a status report.
    pub(crate) name: &'static str,
    /// The environment variable that overrides it.
    pub(crate) variable: &'static str,
    /// The value in force now.
    pub(crate) value: Duration,
    /// The value used when the variable is unset or unusable.
    pub(crate) default: Duration,
    /// The ceiling a configured value is clamped to.
    pub(crate) max: Duration,
    /// Whether an operator set the value in force.
    pub(crate) overridden: bool,
}

impl Deadline {
    /// Resolve one deadline from its environment variable.
    fn resolve(
        name: &'static str,
        variable: &'static str,
        unit: Unit,
        default: Duration,
        max: Duration,
    ) -> Self {
        let configured = std::env::var(variable).ok();
        Self::resolve_from(name, variable, unit, default, max, configured.as_deref())
    }

    /// Resolve a deadline from an already-read value.
    ///
    /// Split from the environment-reading entry point so the parsing and the
    /// clamp are testable without mutating the process environment, which every
    /// other test in the binary shares.
    fn resolve_from(
        name: &'static str,
        variable: &'static str,
        unit: Unit,
        default: Duration,
        max: Duration,
        configured: Option<&str>,
    ) -> Self {
        let mut deadline = Self {
            name,
            variable,
            value: default,
            default,
            max,
            overridden: false,
        };
        let Some(raw) = configured else {
            return deadline;
        };
        match parse_amount(raw, unit) {
            Some(amount) if !amount.is_zero() => {
                deadline.value = amount.min(max);
                deadline.overridden = true;
            }
            // A value that does not parse, or that is zero, is ignored rather
            // than honoured: silently shortening a wait to nothing would turn a
            // slow host into a failed start with no explanation.
            _ => warn_once(variable, raw, deadline.default),
        }
        deadline
    }

    /// The wait itself.
    pub(crate) fn get(self) -> Duration {
        self.value
    }

    /// How this deadline reads in an error an operator has to act on.
    ///
    /// A timeout message is only useful if it says which bound was hit and what
    /// can be done about it. The rendering names the variable that overrides the
    /// value and the ceiling that caps it, so the operator does not have to find
    /// the table to learn either. The host-command deadline is the one that
    /// matters most here: it is resolved for every host command, so a host under
    /// heavy load hits it first and its variable is the one an operator raises.
    pub(crate) fn describe_timeout(self) -> String {
        format!("{:?} ({}, max {:?})", self.value, self.variable, self.max)
    }

    /// Render the deadline for a status report.
    pub(crate) fn describe(self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "variable": self.variable,
            "value_ms": self.value.as_millis(),
            "default_ms": self.default.as_millis(),
            "max_ms": self.max.as_millis(),
            "overridden": self.overridden,
        })
    }
}

fn parse_amount(raw: &str, unit: Unit) -> Option<Duration> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let amount = trimmed.parse::<u64>().ok()?;
    Some(match unit {
        Unit::Milliseconds => Duration::from_millis(amount),
        Unit::Seconds => Duration::from_secs(amount),
    })
}

/// How long a launched runtime gets to publish its pid and ready files.
pub(crate) fn session_ready() -> Deadline {
    Deadline::resolve(
        "session_ready",
        "ENCLAVE_SESSION_READY_MS",
        Unit::Milliseconds,
        Duration::from_secs(5),
        Duration::from_secs(120),
    )
}

/// How long a runtime that has been forked but has not finished the `exec` that
/// makes it the runtime gets to become recognizable.
///
/// A stop reads the command line of the pid a record names to refuse a pid that is
/// not an Enclave runtime. Between the fork that creates a runtime and the end of
/// its `exec` there is a moment where that command line is still the launcher's or
/// reads as empty, and a record read in that moment would be called stale and
/// cleared while the runtime kept running. This is the window the stop waits out
/// before it gives up on a record; it is not a wait a healthy stop pays, because a
/// runtime that is already recognizable costs nothing.
pub(crate) fn runtime_exec_settle() -> Deadline {
    Deadline::resolve(
        "runtime_exec_settle",
        "ENCLAVE_RUNTIME_EXEC_SETTLE_MS",
        Unit::Milliseconds,
        Duration::from_millis(500),
        Duration::from_secs(30),
    )
}

/// How long a runtime gets to exit after SIGTERM before it is killed.
pub(crate) fn runtime_term_grace() -> Deadline {
    Deadline::resolve(
        "runtime_term_grace",
        "ENCLAVE_RUNTIME_TERM_GRACE_MS",
        Unit::Milliseconds,
        Duration::from_millis(500),
        Duration::from_secs(30),
    )
}

/// How long a killed runtime gets to disappear before the stop reports failure.
pub(crate) fn runtime_kill_grace() -> Deadline {
    Deadline::resolve(
        "runtime_kill_grace",
        "ENCLAVE_RUNTIME_KILL_GRACE_MS",
        Unit::Milliseconds,
        Duration::from_millis(500),
        Duration::from_secs(30),
    )
}

/// How long a workspace image's loop device gets to detach.
pub(crate) fn loop_detach() -> Deadline {
    Deadline::resolve(
        "loop_detach",
        "ENCLAVE_LOOP_DETACH_MS",
        Unit::Milliseconds,
        Duration::from_secs(2),
        Duration::from_secs(60),
    )
}

/// How long the persistent command helper gets to accept a connection.
pub(crate) fn helper_start() -> Deadline {
    Deadline::resolve(
        "helper_start",
        "ENCLAVE_HELPER_START_MS",
        Unit::Milliseconds,
        Duration::from_secs(5),
        Duration::from_secs(60),
    )
}

/// The default deadline for one host command.
///
/// This is the bound for every host command that does not set its own, which
/// includes the network and storage setup commands.
pub(crate) fn host_command() -> Deadline {
    Deadline::resolve(
        "host_command",
        "ENCLAVE_HOST_COMMAND_TIMEOUT_SECS",
        Unit::Seconds,
        HOST_COMMAND_DEFAULT,
        HOST_COMMAND_MAX,
    )
}

/// How long daemon shutdown waits for running operations before leaving them.
pub(crate) fn daemon_shutdown_grace() -> Deadline {
    Deadline::resolve(
        "daemon_shutdown_grace",
        "ENCLAVE_SHUTDOWN_GRACE_SECS",
        Unit::Seconds,
        Duration::from_secs(30),
        Duration::from_secs(600),
    )
}

/// Every deadline, for a status report that shows what is actually in force.
pub(crate) fn describe_all() -> serde_json::Value {
    serde_json::Value::Array(
        [
            session_ready(),
            runtime_exec_settle(),
            runtime_term_grace(),
            runtime_kill_grace(),
            loop_detach(),
            helper_start(),
            host_command(),
            daemon_shutdown_grace(),
        ]
        .iter()
        .map(|deadline| deadline.describe())
        .collect(),
    )
}

#[cfg(test)]
#[path = "../tests/src/deadlines.rs"]
mod tests;
