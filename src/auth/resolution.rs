//! Which credentials a workspace is entitled to.
//!
//! Two declarations end up here. An `auth` provider names one of the fixed
//! providers, and the wrapper inside the workspace exports it from the provider
//! table. An `env_token` names the variable the workspace asks for, which is a
//! provider's variable or a name the provider table has never heard of; its value
//! comes from the store slot that name derives. Downstream they are the same
//! thing — a name, a variable, and a value — so they resolve to the same type.
//!
//! Resolution is read-only and takes the state directory rather than an
//! `AuthManager`, because it answers a question about the store instead of
//! changing it. It is kept apart from the facade for that reason: the store
//! operations and the question "what would this workspace be given" are two
//! different concerns, and the answer to the second depends on the workspace's
//! owner rather than on the caller.

use std::path::Path;

use anyhow::Result;

use super::names;
use super::providers;
use super::scope::TokenScope;
use super::storage;

/// One credential, resolved and ready to be written into a workspace.
///
/// `name` is what the store is keyed by and what the audit log records: a
/// provider's name, or the slot an environment token reads from. It is the
/// credential's identity, so the same credential declared two ways is one name
/// and one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceAuthToken {
    pub name: String,
    /// The variable the workspace sees the value as, and the file name the
    /// wrapper reads it from.
    pub env_var: String,
    pub token: String,
}

/// The workspace a credential is being resolved for.
///
/// The ids travel with the owner because an audit event about a credential
/// reaching a workspace has to name the workspace and the namespace it came from,
/// and splitting them across arguments invites recording one without the other.
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

/// The provider tokens a workspace's `auth` declaration resolves to.
pub(super) fn provider_tokens(
    state_dir: &Path,
    owner: Option<&str>,
    auth_providers: &[String],
) -> Vec<WorkspaceAuthToken> {
    resolve_providers(state_dir, owner, auth_providers, Report::Warn)
}

/// The two channels a workspace's declarations resolve to, as they are injected.
///
/// A provider token goes to the workspace's auth directory, where the wrapper's
/// provider loop reads it, and an environment token to its env directory, where
/// the wrapper exports it under its own name. The split is the caller's business
/// because it is what the files differ in; the resolution is not.
pub(super) fn injectable(
    state_dir: &Path,
    owner: Option<&str>,
    auth_providers: &[String],
    env_tokens: &[String],
) -> (Vec<WorkspaceAuthToken>, Vec<WorkspaceAuthToken>) {
    (
        resolve_providers(state_dir, owner, auth_providers, Report::Warn),
        resolve_env_tokens(state_dir, owner, env_tokens, Report::Warn),
    )
}

/// Every credential a workspace's two declarations resolve to, deduplicated.
///
/// This is the read-only view of the same resolution a start performs, and it is
/// what a caller uses to ask what a workspace would be given. It does not warn:
/// it runs on every command, where a workspace that simply has nothing stored
/// would repeat the same line until it buried the warnings that matter.
pub(super) fn credentials(
    state_dir: &Path,
    owner: Option<&str>,
    auth_providers: &[String],
    env_tokens: &[String],
) -> Vec<WorkspaceAuthToken> {
    merge_channels(
        resolve_providers(state_dir, owner, auth_providers, Report::Quiet),
        resolve_env_tokens(state_dir, owner, env_tokens, Report::Quiet),
    )
}

fn resolve_providers(
    state_dir: &Path,
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

        match storage::load_token(state_dir, &scope, provider) {
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

fn resolve_env_tokens(
    state_dir: &Path,
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

        match storage::load_token(state_dir, &scope, &slot) {
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

/// One list from the two injection channels.
///
/// A provider declared both as an `auth` provider and as an `env_token` is one
/// credential: the same value from the same slot, written twice because the two
/// declarations are two channels, but one credential to scrub and one event to
/// record.
pub(super) fn merge_channels(
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
