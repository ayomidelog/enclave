//! The authorization decision for one request.

use std::path::Path;

use anyhow::{bail, Result};

use super::decision::evaluate_policy_decision;
use super::store::load_policy;

/// The actions that are not authorized.
///
/// An action belongs here only if it reads state or decides the policy itself.
/// Everything that changes host state goes through [`authorize`]. The list is an
/// array rather than a `matches!` so that adding to it is a visible edit with a
/// test that fails until the new entry is justified, because the failure mode of a
/// name arriving here by accident is an action that any local user can run.
pub(super) const POLICY_EXEMPT_ACTIONS: &[&str] = &[
    "ping",
    "shutdown",
    "daemon.health",
    "policy.get",
    "policy.set_default",
    "policy.allow",
    "policy.deny",
    "policy.clear",
];

pub(super) fn is_policy_exempt(action: &str) -> bool {
    POLICY_EXEMPT_ACTIONS.contains(&action)
}

pub fn authorize(state_dir: &Path, uid: u32, action: &str) -> Result<()> {
    if uid == 0 {
        tracing::info!("policy audit: allowing action '{}' for root uid 0", action);
        return Ok(());
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
