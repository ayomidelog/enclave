//! The authorization decision for one request.

use std::path::Path;

use anyhow::{bail, Result};

use super::decision::evaluate_policy_decision;
use super::store::load_policy;

/// The actions that are not authorized.
///
/// An action belongs here only if it reads state and changes nothing. Everything
/// that changes host state goes through [`authorize`]. The list is an array rather
/// than a `matches!` so that adding to it is a visible edit with a test that fails
/// until the new entry is justified, because the failure mode of a name arriving
/// here by accident is an action that any local user can run.
pub(super) const POLICY_EXEMPT_ACTIONS: &[&str] =
    &["ping", "shutdown", "daemon.health", "policy.get"];

/// The actions only root may run, whoever the policy says otherwise.
///
/// These are the actions that decide the policy. Letting them be authorized by the
/// policy they edit is circular: a rule that allowed `policy.allow` would let its
/// holder grant itself every other action, so the check has to be independent of the
/// file. Root is the answer because root is already unconditionally allowed every
/// other action, so this adds no restriction an operator would notice. It is not the
/// only boundary — the socket is created `0600` and owned by root, so a non-root
/// process cannot connect at all — but it is the one that keeps the policy file from
/// being the way around itself if that ever changes.
pub(super) const ROOT_ONLY_ACTIONS: &[&str] = &[
    "policy.set_default",
    "policy.allow",
    "policy.deny",
    "policy.clear",
];

pub(super) fn is_policy_exempt(action: &str) -> bool {
    POLICY_EXEMPT_ACTIONS.contains(&action)
}

pub(super) fn is_root_only(action: &str) -> bool {
    ROOT_ONLY_ACTIONS.contains(&action)
}

pub fn authorize(state_dir: &Path, uid: u32, action: &str) -> Result<()> {
    if uid == 0 {
        tracing::info!("policy audit: allowing action '{}' for root uid 0", action);
        return Ok(());
    }

    if is_root_only(action) {
        bail!(
            "policy denied action '{}' for uid {}: this action changes the policy itself and is restricted to root",
            action,
            uid
        );
    }

    if is_policy_exempt(action) {
        return Ok(());
    }

    let policy = load_policy(state_dir)?;
    match evaluate_policy_decision(&policy, uid, action) {
        Some(true) => Ok(()),
        Some(false) => bail!("policy denied action '{}' for uid {}", action, uid),
        None if policy.default_allow => Ok(()),
        None => bail!(
            "policy denied action '{}' for uid {} (default deny)",
            action,
            uid
        ),
    }
}
