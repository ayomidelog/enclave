//! Resolving the credentials a workspace is entitled to.
//!
//! This is the facade the rest of the tree uses. It answers one question — which
//! credentials does this workspace get — and the answer depends on the
//! workspace's owner, so the resolution lives here rather than at the call site
//! that happens to be starting a workspace.
//!
//! Two declarations end up here. An `auth` provider names one of the fixed
//! providers, and the wrapper inside the workspace exports it from the provider
//! table. An `env_token` names the variable the workspace asks for, which is a
//! provider's variable or a name the provider table has never heard of; its value
//! comes from the store slot that name derives. Downstream they are the same
//! thing — a name, a variable, and a value — so they resolve to the same type.

use std::path::PathBuf;

use anyhow::Result;

use super::audit::{self, AuditAction, AuditEvent};
use super::names;
use super::scope::TokenScope;
use super::storage::{self, StoreOutcome, StoredToken};
use super::{inject, providers};

/// One credential, resolved and ready to be written into a workspace.
///
/// `name` is what the store is keyed by and what the audit log records: a
/// provider's name, or the slot a free-form environment token reads from. It is
/// the credential's identity, so the same credential declared two ways is one
/// name and one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceAuthToken {
    pub name: String,
    /// The variable the workspace sees the value as, and the file name the
    /// wrapper reads it from.
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

/// How much a credential that did not resolve should say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Report {
    /// Log it. A start resolves once, so a workspace that comes up without a
    /// credential it asked for is worth knowing about.
    Warn,
    /// Say nothing. A command resolves on every exec, so the same warning would
    /// repeat for a workspace that simply has nothing stored, and the repetition
    /// would bury the warnings that matter.
    Quiet,
}

impl Report {
    fn warns(self) -> bool {
        matches!(self, Self::Warn)
    }
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
            self.resolve_credentials(target.owner, auth_providers, env_tokens, Report::Warn);
        inject::write_workspace_auth(workspace_rootfs, &providers, &env)?;
        Ok(merge_channels(providers, env))
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
        self.provider_tokens(owner, auth_providers, Report::Warn)
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
        let (providers, env) =
            self.resolve_credentials(owner, auth_providers, env_tokens, Report::Quiet);
        merge_channels(providers, env)
    }

    /// The credentials a workspace's declarations resolve to, split by how they
    /// are injected: a provider token is written to the workspace's auth
    /// directory, where the wrapper's provider loop reads it, and an environment
    /// token to its env directory, where the wrapper exports it under its own
    /// name.
    fn resolve_credentials(
        &self,
        owner: Option<&str>,
        auth_providers: &[String],
        env_tokens: &[String],
        report: Report,
    ) -> (Vec<WorkspaceAuthToken>, Vec<WorkspaceAuthToken>) {
        (
            self.provider_tokens(owner, auth_providers, report),
            self.env_tokens(owner, env_tokens, report),
        )
    }

    fn provider_tokens(
        &self,
        owner: Option<&str>,
        auth_providers: &[String],
        report: Report,
    ) -> Vec<WorkspaceAuthToken> {
        let scope = TokenScope::for_owner(owner);
        let mut tokens = Vec::new();
        for provider in auth_providers {
            let Some(env_var) = providers::provider_env_var(provider) else {
                if report.warns() {
                    tracing::warn!(
                        "workspace requested unsupported auth provider '{}'; skipping",
                        provider
                    );
                }
                continue;
            };

            match storage::load_token(&self.state_dir, &scope, provider) {
                Ok(Some(token)) => tokens.push(WorkspaceAuthToken {
                    name: provider.clone(),
                    env_var: env_var.to_string(),
                    token,
                }),
                Ok(None) if report.warns() => {
                    tracing::warn!(
                        "no auth token stored for provider '{}'; the workspace will start without it",
                        provider
                    );
                }
                Err(err) if report.warns() => {
                    tracing::warn!(
                        "failed to load the auth token for provider '{}': {err:#}; skipping it",
                        provider
                    );
                }
                _ => {}
            }
        }
        tokens
    }

    fn env_tokens(
        &self,
        owner: Option<&str>,
        env_tokens: &[String],
        report: Report,
    ) -> Vec<WorkspaceAuthToken> {
        let scope = TokenScope::for_owner(owner);
        let mut tokens = Vec::new();
        for env_var in env_tokens {
            let slot = match env_token_slot(env_var) {
                Ok(slot) => slot,
                Err(err) => {
                    if report.warns() {
                        tracing::warn!(
                            "workspace requested an unusable environment token '{}': {err:#}; skipping",
                            env_var
                        );
                    }
                    continue;
                }
            };

            match storage::load_token(&self.state_dir, &scope, &slot) {
                Ok(Some(token)) => tokens.push(WorkspaceAuthToken {
                    name: slot,
                    env_var: env_var.clone(),
                    token,
                }),
                Ok(None) if report.warns() => {
                    tracing::warn!(
                        "no token stored for environment token '{}' (slot '{}'); the workspace will start without it",
                        env_var,
                        slot
                    );
                }
                Err(err) if report.warns() => {
                    tracing::warn!(
                        "failed to load environment token '{}' (slot '{}'): {err:#}; skipping it",
                        env_var,
                        slot
                    );
                }
                _ => {}
            }
        }
        tokens
    }
}

/// The store slot an environment token reads its value from.
///
/// A name that is one of the providers' variables reads that provider's token:
/// that is what the name meant before free-form names existed, so a workspace
/// declaring `env_tokens = ["GITHUB_TOKEN"]` keeps resolving. Any other
/// well-formed name reads the slot derived from the name itself, which is what
/// makes a credential the provider table has never heard of injectable.
fn env_token_slot(env_var: &str) -> Result<String> {
    match providers::provider_for_env_var(env_var) {
        Some(provider) => Ok(provider.to_string()),
        None => names::slot_for_env_token(env_var),
    }
}

/// The names to record for credentials that reached a workspace.
fn names(tokens: &[WorkspaceAuthToken]) -> Vec<String> {
    tokens.iter().map(|token| token.name.clone()).collect()
}

/// One list from the two injection channels.
///
/// A provider declared both as an `auth` provider and as an `env_token` is one
/// credential: the same value from the same slot, written twice because the two
/// declarations are two channels, but one credential to scrub and one event to
/// record.
fn merge_channels(
    providers: Vec<WorkspaceAuthToken>,
    env: Vec<WorkspaceAuthToken>,
) -> Vec<WorkspaceAuthToken> {
    let mut credentials = providers;
    for token in env {
        if !credentials
            .iter()
            .any(|existing| existing.name == token.name)
        {
            credentials.push(token);
        }
    }
    credentials
}
