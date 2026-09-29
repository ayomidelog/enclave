//! Resolving the tokens a workspace is entitled to.
//!
//! This is the facade the rest of the tree uses. It answers one question —
//! which tokens does this workspace get — and the answer depends on the
//! workspace's owner, so the resolution lives here rather than at the call site
//! that happens to be starting a workspace.

use std::path::PathBuf;

use anyhow::Result;

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
        storage::store_token(&self.state_dir, scope, provider, token, force)
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
        storage::delete_token(&self.state_dir, scope, provider)
    }

    /// Resolve this workspace's tokens and write them into its namespace.
    pub fn sync_workspace_auth(
        &self,
        workspace_rootfs: &str,
        owner: Option<&str>,
        auth_providers: &[String],
        env_tokens: &[String],
    ) -> Result<Vec<WorkspaceAuthToken>> {
        let tokens = self.resolve_tokens(owner, auth_providers);
        let env = self.resolve_env_tokens(owner, env_tokens);
        inject::write_workspace_auth(workspace_rootfs, &tokens, &env)?;
        Ok(tokens)
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
