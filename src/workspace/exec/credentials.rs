//! The credentials a command runs with, and keeping them out of its output.
//!
//! Both halves are about the same thing: what a command inside a workspace was
//! given, and where that value may travel afterwards. The refresh is what makes
//! the answer current, and the scrub is what stops the answer leaving with the
//! output.

use std::path::Path;

use anyhow::{anyhow, Result};

use super::WorkspaceMetadata;

/// Re-resolve the workspace's credentials and write them into its namespace.
///
/// A failure here fails the command rather than running it with whatever the
/// workspace happens to hold: the reason to rewrite the files is that the store
/// may have changed, and a command that ran with a credential the store no longer
/// has is the case this exists to prevent.
pub(super) fn refresh(
    state_dir: &Path,
    workspace: &WorkspaceMetadata,
    runtime_pid: u32,
) -> Result<Vec<crate::auth::WorkspaceAuthToken>> {
    let manager = crate::auth::AuthManager::new(state_dir.to_path_buf());
    let target = crate::auth::WorkspaceAuthTarget {
        sandbox_id: &workspace.sandbox_id,
        workspace_id: &workspace.id,
        owner: workspace.owner.as_deref(),
    };
    let workspace_rootfs = format!("/proc/{runtime_pid}/root");
    manager.inject_workspace_auth(
        &workspace_rootfs,
        &target,
        &workspace.auth_providers,
        &workspace.env_tokens,
    )
}

/// Replace every injected value in `text` with `[REDACTED]`.
///
/// The values are the ones the command was actually run with, so a workspace is
/// scrubbed of the credentials it held rather than of the ones it was configured
/// with. This is separate from the audit below because the two answer different
/// questions: a caller can ask to see the unscrubbed output, but nothing in the tree
/// may write a token into a file, so the log is always given the scrubbed copy.
pub(super) fn redact(credentials: &[crate::auth::WorkspaceAuthToken], text: &str) -> String {
    if credentials.is_empty() {
        return text.to_string();
    }
    let secrets: Vec<String> = credentials
        .iter()
        .map(|credential| credential.token.clone())
        .collect();
    crate::auth::scrub_secrets(text, &secrets)
}

/// Record that each credential reached the command.
///
/// This is the event an operator asking "when was this credential used" wants. It is
/// recorded whether or not the value appeared in the output, because the credential
/// was available to the command either way, and it is recorded for an unscrubbed
/// command too, because the credential was still used.
pub(super) fn record_injection(
    state_dir: &Path,
    workspace: &WorkspaceMetadata,
    credentials: &[crate::auth::WorkspaceAuthToken],
) {
    if credentials.is_empty() {
        return;
    }
    let manager = crate::auth::AuthManager::new(state_dir.to_path_buf());
    let target = crate::auth::WorkspaceAuthTarget {
        sandbox_id: &workspace.sandbox_id,
        workspace_id: &workspace.id,
        owner: workspace.owner.as_deref(),
    };
    let names: Vec<String> = credentials
        .iter()
        .map(|credential| credential.name.clone())
        .collect();
    if let Err(err) = manager.audit_inject(&target, &names) {
        // A broken audit trail is reported rather than turned into a failed command:
        // the command ran, and the credential was used either way.
        tracing::warn!(
            "failed to record the auth injection for workspace {}: {err:#}",
            workspace.id
        );
    }
}

/// The runtime pid a command has to run against, or why it cannot.
pub(super) fn running_runtime_pid(workspace: &WorkspaceMetadata) -> Result<u32> {
    workspace.runtime_pid.ok_or_else(|| {
        anyhow!(
            "workspace '{}' has no runtime pid; restart workspace",
            workspace.id
        )
    })
}
