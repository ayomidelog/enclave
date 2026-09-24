use super::primitives::detect_iptables;
use super::*;

/// A firewall rule whose comment proves Enclave installed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnedRule {
    pub(crate) chain: String,
    pub(crate) owner: String,
    pub(crate) rule: String,
}

/// Enumerate firewall rules that Enclave tagged as its own.
pub(crate) fn list_owned_rules() -> Result<Vec<OwnedRule>> {
    let iptables = detect_iptables()?;
    let output = Command::new(&iptables)
        .arg("-S")
        .output()
        .with_context(|| format!("failed to list firewall rules via {iptables}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "listing firewall rules via {} failed ({}): {}",
            iptables,
            output.status,
            stderr.trim()
        );
    }
    Ok(parse_owned_rules(&String::from_utf8_lossy(&output.stdout)))
}

pub(in crate::network) fn parse_owned_rules(output: &str) -> Vec<OwnedRule> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("-A ")?;
            let (chain, rule) = rest.split_once(' ')?;
            let owner = owned_rule_owner(rule)?;
            Some(OwnedRule {
                chain: chain.to_string(),
                owner,
                rule: rule.to_string(),
            })
        })
        .collect()
}

pub(in crate::network) fn owned_rule_owner(rule: &str) -> Option<String> {
    let (_, comment) = rule.split_once("--comment")?;
    let comment = comment.trim_start();
    let comment = comment.strip_prefix('"').unwrap_or(comment);
    let comment = comment
        .split_once('"')
        .map(|(value, _)| value)
        .unwrap_or(comment);
    let comment = comment.split_whitespace().next().unwrap_or_default();
    let owner = comment.strip_prefix(RULE_COMMENT_PREFIX)?;
    if owner.is_empty() {
        return None;
    }
    Some(owner.to_string())
}
