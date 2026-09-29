//! Resolving the tokens a workspace is entitled to.
//!
//! This is the facade the rest of the tree uses. It answers one question —
//! which tokens does this workspace get — and the answer depends on the
//! workspace's owner, so the resolution lives here rather than at the call site
//! that happens to be starting a workspace.

use std::path::PathBuf;

use anyhow::Result;

use super::audit::{self, AuditAction, AuditEvent};
use super::scope::TokenScope;
use super::storage::{self, StoreOutcome, StoredToken};
use super::{inject, providers};

/// One provider's token, resolved and ready to be written into a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceAuthToken {
    pub provider: String,
    pub env_var: String,
    pub token: String,
}

/// The workspace a token is being resolved for.
///
/// The ids travel with the owner because an audit event about a token reaching a
/// workspace has to name the workspace and the namespace it came from, and
/// splitting them across arguments invites recording one without the other.
#[derive(Debug, Clone, Copy)]
pub struct WorkspaceAuthTarget<'a> {
    pub sandbox_id: &'a str,
    pub workspace_id: &'a str,
    pub owner: Option<&'a str>,
}

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
        provider: &str,
        token: &str,
        force: bool,
    ) -> Result<StoreOutcome> {
        let outcome = storage::store_token(&self.state_dir, scope, provider, token, force)?;
        if outcome.stored() {
            audit::record(
                &self.state_dir,
                &AuditEvent::for_namespace(AuditAction::Store, scope.user_id(), provider),
            )?;
        }
        Ok(outcome)
    }

    pub fn list_tokens(&self, scope: &TokenScope) -> Result<Vec<StoredToken>> {
        storage::list_tokens(&self.state_dir, scope)
    }

    pub fn token_exists(&self, scope: &TokenScope, provider: &str) -> Result<bool> {
        storage::token_exists(&self.state_dir, scope, provider)
    }

    pub fn load_token(&self, scope: &TokenScope, provider: &str) -> Result<Option<String>> {
        storage::load_token(&self.state_dir, scope, provider)
    }

    pub fn delete_token(&self, scope: &TokenScope, provider: &str) -> Result<bool> {
        let removed = storage::delete_token(&self.state_dir, scope, provider)?;
        if removed {
            audit::record(
                &self.state_dir,
                &AuditEvent::for_namespace(AuditAction::Revoke, scope.user_id(), provider),
            )?;
        }
        Ok(removed)
    }

    /// Resolve this workspace's tokens and write them into its namespace.
    pub fn sync_workspace_auth(
        &self,
        workspace_rootfs: &str,
        target: &WorkspaceAuthTarget<'_>,
        auth_providers: &[String],
        env_tokens: &[String],
    ) -> Result<Vec<WorkspaceAuthToken>> {
        let tokens = self.resolve_tokens(target.owner, auth_providers);
        let env = self.resolve_env_tokens(target.owner, env_tokens);
        inject::write_workspace_auth(workspace_rootfs, &tokens, &env)?;
        // Audited after the write, so the log describes injections that
        // happened rather than ones that were attempted.
        for token in &tokens {
            self.audit_inject(target, &token.provider)?;
        }
        Ok(tokens)
    }

    /// Record that a token reached a workspace.
    ///
    /// The daemon's exec path calls this too: there the token is already in the
    /// workspace and is being handed to a specific command, which is the event
    /// an operator asking "when was this credential used" wants.
    pub fn audit_inject(&self, target: &WorkspaceAuthTarget<'_>, provider: &str) -> Result<()> {
        audit::record(
            &self.state_dir,
            &AuditEvent::for_workspace(
                AuditAction::Inject,
                target.owner,
                provider,
                target.sandbox_id,
                target.workspace_id,
            ),
        )
    }

    /// The tokens a workspace would be given, without writing them anywhere.
    ///
    /// Used where the values are needed but not injected: the exec path scrubs
    /// the tokens a workspace holds out of its captured output, and it has to
    /// resolve them the same way the start did, or it would scrub the wrong
    /// secret.
    pub fn resolve_tokens(
        &self,
        owner: Option<&str>,
        auth_providers: &[String],
    ) -> Vec<WorkspaceAuthToken> {
        let scope = TokenScope::for_owner(owner);
        let mut tokens = Vec::new();
        for provider in auth_providers {
            let Some(env_var) = providers::provider_env_var(provider) else {
                tracing::warn!(
                    "workspace requested unsupported auth provider '{}'; skipping",
                    provider
                );
                continue;
            };

            match storage::load_token(&self.state_dir, &scope, provider) {
                Ok(Some(token)) => tokens.push(WorkspaceAuthToken {
                    provider: provider.clone(),
                    env_var: env_var.to_string(),
                    token,
                }),
                Ok(None) => {
                    tracing::warn!(
                        "no auth token configured for provider '{}'; workspace will start without it",
                        provider
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        "failed to load auth token for provider '{}': {err:#}; skipping provider",
                        provider
                    );
                }
            }
        }
        tokens
    }

    /// The tokens a workspace holds, without the warnings `resolve_tokens` emits.
    ///
    /// This runs on every command rather than once per start, so a provider with
    /// nothing stored would otherwise warn on every exec. A token that cannot be
    /// read is skipped for the same reason: it is not in the workspace either, so
    /// there is nothing to scrub.
    pub fn tokens_for_command(
        &self,
        owner: Option<&str>,
        auth_providers: &[String],
    ) -> Vec<WorkspaceAuthToken> {
        let scope = TokenScope::for_owner(owner);
        auth_providers
            .iter()
            .filter_map(|provider| {
                let env_var = providers::provider_env_var(provider)?;
                let token = storage::load_token(&self.state_dir, &scope, provider)
                    .ok()
                    .flatten()?;
                Some(WorkspaceAuthToken {
                    provider: provider.clone(),
                    env_var: env_var.to_string(),
                    token,
                })
            })
            .collect()
    }

    fn resolve_env_tokens(
        &self,
        owner: Option<&str>,
        env_tokens: &[String],
    ) -> Vec<(String, String)> {
        let scope = TokenScope::for_owner(owner);
        let mut tokens = Vec::new();
        for env_var in env_tokens {
            let Some(provider) = providers::provider_for_env_var(env_var) else {
                tracing::warn!(
                    "workspace requested unsupported environment token '{}'; skipping",
                    env_var
                );
                continue;
            };

            match storage::load_token(&self.state_dir, &scope, provider) {
                Ok(Some(token)) => tokens.push((env_var.clone(), token)),
                Ok(None) => {
                    tracing::warn!(
                        "no token configured for environment token '{}'; workspace will start without it",
                        env_var
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        "failed to load environment token '{}': {err:#}; skipping token",
                        env_var
                    );
                }
            }
        }
        tokens
    }
}
