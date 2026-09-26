//! Changing the policy: the default, and the allow and deny rules.

use std::path::Path;

use anyhow::{bail, Result};

use super::super::types::{Policy, PolicyRule};
use super::store::with_policy_mut;

pub fn set_default_allow(state_dir: &Path, default_allow: bool) -> Result<Policy> {
    with_policy_mut(state_dir, |policy| {
        policy.default_allow = default_allow;
        Ok(policy.clone())
    })
}

pub fn add_allow_rule(state_dir: &Path, uid: Option<u32>, action: &str) -> Result<Policy> {
    upsert_rule(state_dir, uid, action, true)
}

pub fn add_deny_rule(state_dir: &Path, uid: Option<u32>, action: &str) -> Result<Policy> {
    upsert_rule(state_dir, uid, action, false)
}

pub fn clear_rules(state_dir: &Path, uid: Option<u32>) -> Result<Policy> {
    with_policy_mut(state_dir, |policy| {
        if let Some(target_uid) = uid {
            policy.rules.retain(|rule| rule.uid != Some(target_uid));
        } else {
            policy.rules.clear();
        }
        Ok(policy.clone())
    })
}

fn upsert_rule(state_dir: &Path, uid: Option<u32>, action: &str, is_allow: bool) -> Result<Policy> {
    validate_action_pattern(action)?;
    with_policy_mut(state_dir, |policy| {
        let mut found = false;
        for rule in &mut policy.rules {
            if rule.uid == uid {
                found = true;
                if is_allow {
                    if !rule.allow.iter().any(|x| x == action) {
                        rule.allow.push(action.to_string());
                    }
                    rule.deny.retain(|x| x != action);
                } else {
                    if !rule.deny.iter().any(|x| x == action) {
                        rule.deny.push(action.to_string());
                    }
                    rule.allow.retain(|x| x != action);
                }
                break;
            }
        }

        if !found {
            let mut rule = PolicyRule {
                uid,
                allow: Vec::new(),
                deny: Vec::new(),
            };
            if is_allow {
                rule.allow.push(action.to_string());
            } else {
                rule.deny.push(action.to_string());
            }
            policy.rules.push(rule);
        }
        Ok(policy.clone())
    })
}

fn validate_action_pattern(action: &str) -> Result<()> {
    if action.is_empty() || action.len() > 120 {
        bail!("action pattern must be 1-120 characters");
    }
    for c in action.chars() {
        if !(c.is_ascii_alphanumeric() || c == '.' || c == '*' || c == '_' || c == '-') {
            bail!("action pattern contains invalid character '{}'", c);
        }
    }
    Ok(())
}
