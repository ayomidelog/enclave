use super::primitives::detect_iptables;
use super::*;

use crate::hostcmd::HostCommand;

/// A firewall rule whose comment proves Enclave installed it.
///
/// The table is filled in by the caller that read the dump; the rule text is the
/// body of one `iptables -S` line with the `-A <chain> ` prefix removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnedRule {
    pub(crate) table: String,
    pub(crate) chain: String,
    pub(crate) owner: String,
    pub(crate) rule: String,
}

/// Enumerate firewall rules that Enclave tagged as its own, in every table it
/// installs rules in.
pub(crate) fn list_owned_rules() -> Result<Vec<OwnedRule>> {
    let iptables = detect_iptables()?;
    list_owned_rules_across_tables(&iptables)
}

pub(in crate::network) fn list_owned_rules_across_tables(iptables: &str) -> Result<Vec<OwnedRule>> {
    let mut rules = Vec::new();
    for table in RULE_TABLES {
        rules.extend(list_owned_rules_in_table(iptables, table)?);
    }
    Ok(rules)
}

pub(in crate::network) fn list_owned_rules_in_table(
    iptables: &str,
    table: &str,
) -> Result<Vec<OwnedRule>> {
    let output = HostCommand::new(iptables)
        .args(["-t", table, "-S"])
        .output_cap(RULE_DUMP_CAP)
        .run_checked()
        .with_context(|| format!("failed to list {table} rules via {iptables}"))?;
    Ok(parse_owned_rules(&output.stdout_text())
        .into_iter()
        .map(|rule| OwnedRule {
            table: table.to_string(),
            ..rule
        })
        .collect())
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
                table: String::new(),
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

/// Delete a rule that Enclave owns, identified by the dump it came from.
///
/// The rule text is the body of one `iptables -S` line, so the arguments are
/// rebuilt from it rather than reconstructed from the shape this release
/// installs. `iptables -D` only removes a rule that matches every argument, so a
/// misread line cannot delete an unrelated rule.
pub(crate) fn remove_owned_rule(rule: &OwnedRule) -> Result<()> {
    let iptables = primitives::detect_iptables()?;
    let args = primitives::split_rule_args(&rule.rule);
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    primitives::delete_rule(&iptables, &rule.table, &rule.chain, &args)
}
