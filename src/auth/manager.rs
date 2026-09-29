//! Resolving the tokens a workspace is entitled to.
//!
//! This is the facade the rest of the tree uses. It answers one question —
//! which tokens does this workspace get — and the answer depends on the
//! workspace's owner, so the resolution lives here rather than at the call site
//! that happens to be starting a workspace.

use std::path::PathBuf;

use anyhow::Result;

use super::{inject, providers, storage};

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

    pub fn store_token(&self, provider: &str, token: &str) -> Result<PathBuf> {
        storage::store_token(&self.state_dir, provider, token)
    }

    pub fn list_providers(&self) -> Result<Vec<String>> {
        storage::list_configured_providers(&self.state_dir)
    }

    pub fn token_exists(&self, provider: &str) -> Result<bool> {
        storage::token_exists(&self.state_dir, provider)
    }

    pub fn load_token(&self, provider: &str) -> Result<Option<String>> {
        storage::load_token(&self.state_dir, provider)
    }

    pub fn delete_token(&self, provider: &str) -> Result<bool> {
        storage::delete_token(&self.state_dir, provider)
    }

    /// Resolve this workspace's tokens and write them into its namespace.
    pub fn sync_workspace_auth(
        &self,
        workspace_rootfs: &str,
        auth_providers: &[String],
        env_tokens: &[String],
    ) -> Result<Vec<WorkspaceAuthToken>> {
        let tokens = self.tokens_for_workspace(auth_providers);
        let env = self.env_tokens_for_workspace(env_tokens);
        inject::write_workspace_auth(workspace_rootfs, &tokens, &env)?;
        Ok(tokens)
    }

    fn tokens_for_workspace(&self, auth_providers: &[String]) -> Vec<WorkspaceAuthToken> {
        let mut tokens = Vec::new();
        for provider in auth_providers {
            let Some(env_var) = providers::provider_env_var(provider) else {
                tracing::warn!(
                    "workspace requested unsupported auth provider '{}'; skipping",
                    provider
                );
                continue;
            };

            match storage::load_token(&self.state_dir, provider) {
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

    fn env_tokens_for_workspace(&self, env_tokens: &[String]) -> Vec<(String, String)> {
        let mut tokens = Vec::new();
        for env_var in env_tokens {
            let Some(provider) = providers::provider_for_env_var(env_var) else {
                tracing::warn!(
                    "workspace requested unsupported environment token '{}'; skipping",
                    env_var
                );
                continue;
            };

            match storage::load_token(&self.state_dir, provider) {
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
