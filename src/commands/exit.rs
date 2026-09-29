//! The exit code a command reports, when it is not simply "it failed".
//!
//! A command that reports a specific outcome — a token that already exists, an
//! argument that cannot be used — has to say so in the status a script reads,
//! not only in a message. The code travels as a typed error so the command that
//! knows the outcome raises it and the process boundary is the one place that
//! turns it into a status.

/// A token is already stored for that provider and namespace.
///
/// Distinct from a failure because the caller's next step is different: pass
/// `--force`, or keep the token that is there.
pub const EXIT_TOKEN_EXISTS: i32 = 2;
/// An argument the command cannot use: an unknown provider, a malformed id.
pub const EXIT_INVALID_INPUT: i32 = 3;
/// The command could not read or write what it was asked to.
pub const EXIT_IO: i32 = 4;

/// The status a command exits with when it failed for a reason it did not name.
pub const EXIT_FAILURE: i32 = 1;

#[derive(Debug)]
pub struct CliExit {
    code: i32,
    message: String,
}

impl CliExit {
    /// An error carrying the status the process should exit with.
    pub fn error(code: i32, message: impl Into<String>) -> anyhow::Error {
        anyhow::Error::new(Self {
            code,
            message: message.into(),
        })
    }

    pub fn code(&self) -> i32 {
        self.code
    }
}

impl std::fmt::Display for CliExit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliExit {}

/// The status to exit with for a failure.
///
/// A failure that named its own outcome keeps it; anything else is a plain
/// failure. The chain is searched rather than only the outermost link, because
/// the layers between the command and the process boundary only forward errors.
pub fn exit_code_of(error: &anyhow::Error) -> i32 {
    for cause in error.chain() {
        if let Some(exit) = cause.downcast_ref::<CliExit>() {
            return exit.code();
        }
    }
    EXIT_FAILURE
}

#[cfg(test)]
#[path = "../../tests/src/commands/exit.rs"]
mod tests;
