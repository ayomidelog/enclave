//! The append-only record of every token store, revoke, and inject.
//!
//! A token is a credential the daemon handles on someone's behalf, so who asked
//! for what to happen to it is worth recording. The record is deliberately
//! metadata only: it names the action, the namespace, the provider, and the
//! workspace, and it never contains the token itself. That is what makes the log
//! safe to read and safe to ship somewhere else.
//!
//! One line per event, JSON, appended. Appending rather than rewriting means a
//! reader can follow the file and a writer cannot lose an earlier event, and the
//! line is fsynced so an event that was reported is an event that was recorded.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::Serialize;

const AUDIT_LOG_NAME: &str = "audit.log";

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditAction {
    /// A token was written into a namespace.
    Store,
    /// A token was removed from a namespace.
    Revoke,
    /// A token was handed to a workspace, or to a command run in one.
    Inject,
}

/// One audit line.
///
/// The fields are all metadata. There is no field for the token value, so a
/// value cannot reach the log by a caller filling one in.
#[derive(Debug, Serialize)]
pub struct AuditEvent<'a> {
    pub ts: String,
    pub action: AuditAction,
    /// The namespace's user id, absent for the shared namespace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<&'a str>,
    pub provider: &'a str,
    /// The workspace's sandbox, for an event about a workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<&'a str>,
}

impl<'a> AuditEvent<'a> {
    /// An event about a namespace rather than a workspace: a store or a revoke.
    pub fn for_namespace(action: AuditAction, user: Option<&'a str>, provider: &'a str) -> Self {
        Self {
            ts: timestamp(),
            action,
            user,
            provider,
            sandbox: None,
            workspace: None,
        }
    }

    /// An event about a token reaching a workspace.
    pub fn for_workspace(
        action: AuditAction,
        user: Option<&'a str>,
        provider: &'a str,
        sandbox: &'a str,
        workspace: &'a str,
    ) -> Self {
        Self {
            ts: timestamp(),
            action,
            user,
            provider,
            sandbox: Some(sandbox),
            workspace: Some(workspace),
        }
    }
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Append one event.
///
/// A failure to write is returned rather than swallowed: the caller is deciding
/// whether a credential may be used, and "the audit trail is broken" is not a
/// state to carry on from silently.
pub(super) fn record(state_dir: &Path, event: &AuditEvent<'_>) -> Result<()> {
    let path = audit_log_path(state_dir)?;
    // The log sits beside the tokens, so it is checked the same way they are. An
    // existing file that anyone could have replaced would make every line in it
    // worthless as evidence.
    if path.exists() {
        validate_audit_log(&path)?;
    }
    let mut line = serde_json::to_vec(event).context("failed to encode an audit event")?;
    line.push(b'\n');

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("failed to open audit log {}", path.display()))?;
    file.write_all(&line)
        .with_context(|| format!("failed to append to audit log {}", path.display()))?;
    // An event that was reported has to be an event that was recorded, so the
    // append is flushed to disk before the caller is told it succeeded.
    file.sync_all()
        .with_context(|| format!("failed to flush audit log {}", path.display()))?;
    Ok(())
}

fn audit_log_path(state_dir: &Path) -> Result<PathBuf> {
    let auth_dir = super::storage::ensure_auth_dir(state_dir)?;
    Ok(auth_dir.join(AUDIT_LOG_NAME))
}

fn validate_audit_log(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat audit log {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("refusing to append to symlink audit log {}", path.display());
    }
    if !metadata.is_file() {
        bail!("audit log {} is not a regular file", path.display());
    }
    let expected_uid = unsafe { libc::geteuid() as u32 };
    if metadata.uid() != expected_uid {
        bail!(
            "audit log {} must be owned by uid {}, found uid {}",
            path.display(),
            expected_uid,
            metadata.uid()
        );
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o600 {
        bail!(
            "audit log {} must have mode 0600, found {:o}",
            path.display(),
            mode
        );
    }
    Ok(())
}
