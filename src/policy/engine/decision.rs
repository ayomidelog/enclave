//! Deciding whether a policy allows an action.
//!
//! The rules are two lists, and the decision is made in a fixed order so it does not
//! depend on the order rules were added: a uid-specific rule beats a wildcard one, and
//! a deny beats an allow at the same specificity.

use super::super::types::{Policy, PolicyRule};

pub(super) fn matches_pattern(pattern: &str, action: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return action.starts_with(prefix);
    }
    action == pattern
}

pub(super) fn evaluate_policy_decision(policy: &Policy, uid: u32, action: &str) -> Option<bool> {
    if let Some(decision) = evaluate_rule_group(
        policy.rules.iter().filter(|rule| rule.uid == Some(uid)),
        action,
    ) {
        return Some(decision);
    }
    evaluate_rule_group(
        policy.rules.iter().filter(|rule| rule.uid.is_none()),
        action,
    )
}

pub(super) fn evaluate_rule_group<'a, I>(rules: I, action: &str) -> Option<bool>
where
    I: Iterator<Item = &'a PolicyRule>,
{
    let mut allow_matched = false;
    for rule in rules {
        if rule
            .deny
            .iter()
            .any(|pattern| matches_pattern(pattern, action))
        {
            return Some(false);
        }
        if rule
            .allow
            .iter()
            .any(|pattern| matches_pattern(pattern, action))
        {
            allow_matched = true;
        }
    }
    if allow_matched {
        Some(true)
    } else {
        None
    }
}
