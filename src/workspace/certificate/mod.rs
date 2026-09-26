//! What a workspace cleanup has to prove, and the check that proves it.
//!
//! A cleanup function returning Ok means the calls it made did not report a
//! failure. That is not evidence that the host is clean, so every teardown
//! re-reads the host afterwards and records what it found. The record lives in
//! one module and the checks that fill it in in another, because the callers
//! that only report a cleanup need the type and not the probing.

mod record;
mod verify;

pub use record::WorkspaceCleanupCertificate;
pub(crate) use verify::{verify_workspace_cleanup, verify_workspace_destroyed};

#[cfg(test)]
#[path = "../../../tests/src/workspace/certificate.rs"]
mod tests;
