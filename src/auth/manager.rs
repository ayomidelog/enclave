use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

use super::storage;

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

    pub fn sync_workspace_auth(
        &self,
        workspace_rootfs: &str,
        auth_providers: &[String],
        env_tokens: &[String],
    ) -> Result<Vec<WorkspaceAuthToken>> {
        let rootfs = PathBuf::from(workspace_rootfs);
        if !rootfs.is_absolute() {
            anyhow::bail!(
                "workspace rootfs path must be absolute: {}",
                rootfs.display()
            );
        }
        if !is_workspace_namespace_root(&rootfs) {
            anyhow::bail!(
                "workspace rootfs path must be /proc/<pid>/root: {}",
                rootfs.display()
            );
        }

        let auth_dir = rootfs.join("run/enclave/auth");
        fs::create_dir_all(&auth_dir)
            .with_context(|| format!("failed to create {}", auth_dir.display()))?;
        let env_dir = rootfs.join("run/enclave/env");
        fs::create_dir_all(&env_dir)
            .with_context(|| format!("failed to create {}", env_dir.display()))?;

        for provider in storage::supported_providers() {
            let token_path = storage::token_path_for_provider(&auth_dir, provider)?;
            if token_path.exists() {
                fs::remove_file(&token_path)
                    .with_context(|| format!("failed to remove {}", token_path.display()))?;
            }
        }
        for entry in fs::read_dir(&env_dir)
            .with_context(|| format!("failed to read {}", env_dir.display()))?
        {
            let path = entry?.path();
            if path.is_file() {
                fs::remove_file(&path)
                    .with_context(|| format!("failed to remove {}", path.display()))?;
            }
        }

        let tokens = self.tokens_for_workspace(auth_providers);
        for token in &tokens {
            let token_path = storage::token_path_for_provider(&auth_dir, &token.provider)?;
            crate::fsutil::write_file_atomic(&token_path, token.token.as_bytes(), 0o400)
                .with_context(|| format!("failed to write {}", token_path.display()))?;
        }
        for (env_var, token) in self.env_tokens_for_workspace(env_tokens) {
            let token_path = env_dir.join(&env_var);
            crate::fsutil::write_file_atomic(&token_path, token.as_bytes(), 0o400)
                .with_context(|| format!("failed to write {}", token_path.display()))?;
        }
        Ok(tokens)
    }

    fn tokens_for_workspace(&self, auth_providers: &[String]) -> Vec<WorkspaceAuthToken> {
        let mut tokens = Vec::new();
        for provider in auth_providers {
            let Some(env_var) = storage::provider_env_var(provider) else {
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
            let Some(provider) = storage::provider_for_env_var(env_var) else {
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

fn is_workspace_namespace_root(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::RootDir))
        && matches!(
            components.next(),
            Some(Component::Normal(part)) if part == "proc"
        )
        && matches!(
            components.next(),
            Some(Component::Normal(part)) if is_valid_pid_component(part)
        )
        && matches!(
            components.next(),
            Some(Component::Normal(part)) if part == "root"
        )
        && components.next().is_none()
}

fn is_valid_pid_component(part: &std::ffi::OsStr) -> bool {
    part.to_str()
        .is_some_and(|value| !value.is_empty() && value.chars().all(|c| c.is_ascii_digit()))
}
