//! Machine-readable error codes for the daemon protocol.
//!
//! A client that wants to react to a failure differently from a plain report —
//! retry a conflict, surface a missing sandbox as a usage error, tell an
//! unsupported host apart from a bug — needs to know *what kind* of failure it
//! got. Matching on the message text cannot do that: the wording is for a person
//! reading it, and it changes whenever a message is improved.
//!
//! So a code travels beside the message. The code is attached where the failure
//! is raised, by a function that knows its category, and carried through the
//! `anyhow` error as a typed value rather than being inferred later from the
//! text. A failure that nobody categorized is [`ErrorCode::Internal`], which is
//! the honest answer: something failed that the raising code did not classify.

use serde::{Deserialize, Serialize};

/// The kind of failure, stable across message changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request could not be read or parsed, or a parameter was invalid.
    InvalidRequest,
    /// The named sandbox, workspace, or other resource does not exist.
    NotFound,
    /// The request cannot proceed because of the current state: a name is
    /// ambiguous, a resource already exists, or another operation holds it.
    Conflict,
    /// The caller exceeded its request budget.
    RateLimited,
    /// The caller is not permitted to run this action.
    PolicyDenied,
    /// The host cannot provide something the request needs, such as cgroup v2
    /// or a required host program.
    Unsupported,
    /// A wait hit its deadline before the condition it waited for was met.
    Timeout,
    /// The operation ran but could not release every resource it owns.
    CleanupIncomplete,
    /// Anything the raising code did not classify.
    Internal,
}

impl ErrorCode {
    /// The stable wire and report name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::RateLimited => "rate_limited",
            Self::PolicyDenied => "policy_denied",
            Self::Unsupported => "unsupported",
            Self::Timeout => "timeout",
            Self::CleanupIncomplete => "cleanup_incomplete",
            Self::Internal => "internal",
        }
    }
}

/// An error that names its own category.
///
/// It is carried inside an `anyhow::Error`, so it travels through the layers
/// that only forward errors, and the boundary that answers the client reads the
/// category back with a downcast rather than by reading the message.
#[derive(Debug)]
pub struct Coded {
    code: ErrorCode,
    message: String,
}

impl Coded {
    pub fn code(&self) -> ErrorCode {
        self.code
    }
}

impl std::fmt::Display for Coded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Coded {}

/// Build an error with a known category.
pub fn coded(code: ErrorCode, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Coded {
        code,
        message: message.into(),
    })
}

/// The category of an error, or [`ErrorCode::Internal`] when none was attached.
///
/// A wrapper that only forwards an error preserves the category, so this finds
/// the innermost one. `anyhow` keeps the original error in its chain, so the
/// chain is searched rather than only the outermost link.
pub fn code_of(error: &anyhow::Error) -> ErrorCode {
    for cause in error.chain() {
        if let Some(coded) = cause.downcast_ref::<Coded>() {
            return coded.code();
        }
    }
    ErrorCode::Internal
}

#[cfg(test)]
#[path = "../tests/src/error.rs"]
mod tests;
