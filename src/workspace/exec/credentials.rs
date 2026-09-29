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

/// Replace the credentials the command was given in its captured output.
///
/// The values are the ones the command was actually run with, so a workspace is
/// scrubbed of the credentials it held rather than of the ones it was configured
/// with. Each credential is also recorded, which is the event an operator asking
/// "when was this credential used" wants.
pub(super) fn scrub(
    state_dir: &Path,
    workspace: &WorkspaceMetadata,
    credentials: &[crate::auth::WorkspaceAuthToken],
    stdout: &mut String,
    stderr: &mut String,
) -> Result<()> {
    if credentials.is_empty() {
        return Ok(());
    }
    let secrets: Vec<String> = credentials
        .iter()
        .map(|credential| credential.token.clone())
        .collect();
    *stdout = crate::auth::scrub_secrets(stdout, &secrets);
    *stderr = crate::auth::scrub_secrets(stderr, &secrets);

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
        // The output has already been scrubbed by this point, so a broken audit
        // trail is reported rather than turned into a failed command: the caller
        // has the value-safe output either way, and the credential was used.
        tracing::warn!(
            "failed to record the auth injection for workspace {}: {err:#}",
            workspace.id
        );
    }
    Ok(())
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
