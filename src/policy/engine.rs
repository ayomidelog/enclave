use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::types::{Policy, PolicyRule};

pub fn ensure_policy(state_dir: &Path) -> Result<()> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create state dir {}", state_dir.display()))?;
    let path = policy_path(state_dir);
    if path.exists() {
        ensure_policy_permissions(&path)?;
        return Ok(());
    }
    let lock_path = policy_lock_path(state_dir);
    crate::fsutil::with_file_lock(&lock_path, || {
        if path.exists() {
            ensure_policy_permissions(&path)?;
            return Ok(());
        }
        save_policy_unlocked(state_dir, &Policy::default())?;
        ensure_policy_permissions(&path)?;
        Ok(())
    })
}

pub fn load_policy(state_dir: &Path) -> Result<Policy> {
    with_policy(state_dir, |policy| Ok(policy.clone()))
}

fn is_policy_exempt(action: &str) -> bool {
    matches!(
        action,
        "ping"
            | "shutdown"
            | "daemon.health"
            | "policy.get"
            | "policy.set_default"
            | "policy.allow"
            | "policy.deny"
            | "policy.clear"
    )
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

fn policy_path(state_dir: &Path) -> PathBuf {
    state_dir.join("policy.json")
}

fn matches_pattern(pattern: &str, action: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return action.starts_with(prefix);
    }
    action == pattern
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

fn evaluate_policy_decision(policy: &Policy, uid: u32, action: &str) -> Option<bool> {
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

fn evaluate_rule_group<'a, I>(rules: I, action: &str) -> Option<bool>
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

fn with_policy<T, F>(state_dir: &Path, operation: F) -> Result<T>
where
    F: FnOnce(&Policy) -> Result<T>,
{
    ensure_policy(state_dir)?;
    let lock_path = policy_lock_path(state_dir);
    crate::fsutil::with_file_lock(&lock_path, || {
        let policy = load_policy_unlocked(state_dir)?;
        operation(&policy)
    })
}

fn with_policy_mut<T, F>(state_dir: &Path, operation: F) -> Result<T>
where
    F: FnOnce(&mut Policy) -> Result<T>,
{
    ensure_policy(state_dir)?;
    let lock_path = policy_lock_path(state_dir);
    crate::fsutil::with_file_lock(&lock_path, || {
        let mut policy = load_policy_unlocked(state_dir)?;
        let result = operation(&mut policy)?;
        save_policy_unlocked(state_dir, &policy)?;
        Ok(result)
    })
}

fn load_policy_unlocked(state_dir: &Path) -> Result<Policy> {
    let path = policy_path(state_dir);
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read policy {}", path.display()))?;
    let policy: Policy =
        serde_json::from_str(&raw).with_context(|| format!("invalid policy {}", path.display()))?;
    Ok(policy)
}

fn save_policy_unlocked(state_dir: &Path, policy: &Policy) -> Result<()> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create state dir {}", state_dir.display()))?;
    let path = policy_path(state_dir);
    let raw = serde_json::to_string_pretty(policy)?;
    crate::fsutil::write_file_atomic(&path, raw.as_bytes(), 0o600)
        .with_context(|| format!("failed to write policy {}", path.display()))?;
    ensure_policy_permissions(&path)?;
    Ok(())
}

fn ensure_policy_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .permissions();
    let mode = perms.mode() & 0o777;
    if mode != 0o600 {
        perms.set_mode(0o600);
        fs::set_permissions(path, perms)
            .with_context(|| format!("failed to set mode on {}", path.display()))?;
    }
    Ok(())
}

fn policy_lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join("policy.lock")
}

#[cfg(test)]
#[path = "../../tests/src/policy/engine.rs"]
mod tests;
