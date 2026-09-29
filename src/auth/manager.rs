//! Resolving the credentials a workspace is entitled to.
//!
//! This is the facade the rest of the tree uses. It is where a token is stored,
//! listed, read, and removed, and where a workspace's credentials are resolved,
//! written into it, and recorded. Which credentials a workspace gets is decided
//! by [`super::resolution`]; this file is the order the operations happen in and
//! the audit that follows each one.

use std::path::PathBuf;

use anyhow::Result;

use super::audit::{self, AuditAction, AuditEvent};
use super::inject;
use super::resolution;
use super::scope::TokenScope;
use super::storage::{self, StoreOutcome, StoredToken};

pub use super::resolution::{WorkspaceAuthTarget, WorkspaceAuthToken};

#[derive(Debug, Clone)]
pub struct AuthManager {
    state_dir: PathBuf,
}

impl AuthManager {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
        }
    }

    pub fn state_dir(&self) -> &std::path::Path {
        &self.state_dir
    }

    pub fn store_token(
        &self,
        scope: &TokenScope,
        name: &str,
        token: &str,
        force: bool,
    ) -> Result<StoreOutcome> {
        let outcome = storage::store_token(&self.state_dir, scope, name, token, force)?;
        if outcome.stored() {
            audit::record(
                &self.state_dir,
                &AuditEvent::for_namespace(AuditAction::Store, scope.user_id(), name),
            )?;
        }
        Ok(outcome)
    }

    pub fn list_tokens(&self, scope: &TokenScope) -> Result<Vec<StoredToken>> {
        storage::list_tokens(&self.state_dir, scope)
    }

    pub fn token_exists(&self, scope: &TokenScope, name: &str) -> Result<bool> {
        storage::token_exists(&self.state_dir, scope, name)
    }

    pub fn load_token(&self, scope: &TokenScope, name: &str) -> Result<Option<String>> {
        storage::load_token(&self.state_dir, scope, name)
    }

    pub fn delete_token(&self, scope: &TokenScope, name: &str) -> Result<bool> {
        let removed = storage::delete_token(&self.state_dir, scope, name)?;
        if removed {
            audit::record(
                &self.state_dir,
                &AuditEvent::for_namespace(AuditAction::Revoke, scope.user_id(), name),
            )?;
        }
        Ok(removed)
    }

    /// Resolve this workspace's credentials, write them into its namespace, and
    /// record that they reached it.
    pub fn sync_workspace_auth(
        &self,
        workspace_rootfs: &str,
        target: &WorkspaceAuthTarget<'_>,
        auth_providers: &[String],
        env_tokens: &[String],
    ) -> Result<()> {
        let injected =
            self.inject_workspace_auth(workspace_rootfs, target, auth_providers, env_tokens)?;
        // Audited after the write, so the log describes injections that
        // happened rather than ones that were attempted.
        self.audit_inject(target, &names(&injected))?;
        Ok(())
    }

    /// Resolve this workspace's credentials and write them into its namespace.
    ///
    /// Returns what was written. A credential the store had nothing for is not in
    /// the workspace either, so the returned list is what is actually there, and
    /// that is what a caller scrubs out of a command's output and records.
    pub fn inject_workspace_auth(
        &self,
        workspace_rootfs: &str,
        target: &WorkspaceAuthTarget<'_>,
        auth_providers: &[String],
        env_tokens: &[String],
    ) -> Result<Vec<WorkspaceAuthToken>> {
        let (providers, env) =
            resolution::injectable(&self.state_dir, target.owner, auth_providers, env_tokens);
        inject::write_workspace_auth(workspace_rootfs, &providers, &env)?;
        Ok(resolution::merge_channels(providers, env))
    }

    /// Record that tokens reached a workspace.
    ///
    /// The daemon's exec path calls this too: there the token is already in the
    /// workspace and is being handed to a specific command, which is the event
    /// an operator asking "when was this credential used" wants.
    pub fn audit_inject(
        &self,
        target: &WorkspaceAuthTarget<'_>,
        providers: &[String],
    ) -> Result<()> {
        let events: Vec<AuditEvent<'_>> = providers
            .iter()
            .map(|provider| {
                AuditEvent::for_workspace(
                    AuditAction::Inject,
                    target.owner,
                    provider,
                    target.sandbox_id,
                    target.workspace_id,
                )
            })
            .collect();
        audit::record_many(&self.state_dir, &events)
    }

    /// The provider tokens a workspace's `auth` declaration resolves to.
    pub fn resolve_tokens(
        &self,
        owner: Option<&str>,
        auth_providers: &[String],
    ) -> Vec<WorkspaceAuthToken> {
        resolution::provider_tokens(&self.state_dir, owner, auth_providers)
    }

    /// Every credential a workspace's two declarations resolve to.
    ///
    /// This is the read-only view of the same resolution the start performs, and
    /// it is what a caller uses to ask what a workspace would be given. A provider
    /// named both ways appears once, because it is one credential.
    pub fn resolve_workspace_credentials(
        &self,
        owner: Option<&str>,
        auth_providers: &[String],
        env_tokens: &[String],
    ) -> Vec<WorkspaceAuthToken> {
        resolution::credentials(&self.state_dir, owner, auth_providers, env_tokens)
    }
}

/// The names to record for credentials that reached a workspace.
fn names(tokens: &[WorkspaceAuthToken]) -> Vec<String> {
    tokens.iter().map(|token| token.name.clone()).collect()
}
