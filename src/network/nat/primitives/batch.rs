//! Applying several rules in one process.
//!
//! iptables has no batch mode, so a rule per chain cost a process per chain. A restore
//! with --noflush applies a whole table in one process, which is the same work for half
//! the spawns. --noflush is what keeps it additive: without it the restore would replace
//! the table instead of adding to it.
//!
//! An insert and a delete are not symmetric here. An insert of a rule that is already
//! present is tolerated by the kernel, so a batch of inserts is safe to apply blindly.
//! A delete is rejected when the rule is not there and the whole script is abandoned at
//! the first rejected line, so a batch of deletes is only safe for rules a listing has
//! just proved exist, and the caller has to fall back to one delete per rule.

use super::*;

/// Insert several filter rules at the head of their chains in one process.
///
/// iptables has no batch mode, so installing the two anti-spoofing rules took two
/// processes on the workspace start path. iptables-restore with --noflush applies
/// a whole table's worth of rules in one process, which is the same work for half
/// the spawns. The --noflush flag is what keeps it additive: without it the
/// restore would replace the table instead of adding to it.
///
/// Each entry is inserted at position 1, so the entries are written in reverse to
/// leave them in the order given. Like the single-rule form, the caller has to
/// have proven the rules absent already; a duplicate insert is tolerated by the
/// kernel rather than detected here.
pub(in crate::network) fn insert_filter_rules_first(
    iptables: &str,
    rules: &[(&str, &[&str], &str)],
) -> Result<()> {
    if rules.is_empty() {
        return Ok(());
    }

    let mut script = String::from("*filter\n");
    for (chain, rule_args, _) in rules.iter().rev() {
        script.push_str("-I ");
        script.push_str(chain);
        script.push_str(" 1");
        for argument in rule_args.iter() {
            script.push(' ');
            script.push_str(&quote_restore_argument(argument));
        }
        script.push('\n');
    }
    script.push_str("COMMIT\n");

    let restore = restore_binary_for(iptables);
    let descriptions = rules
        .iter()
        .map(|(_, _, description)| *description)
        .collect::<Vec<_>>()
        .join(", ");
    let output = HostCommand::new(&restore)
        .arg("--noflush")
        .stdin(script.into_bytes())
        .run()
        .with_context(|| format!("failed to add {descriptions} via {restore}"))?;
    if !output.success() {
        bail!("failed to add {descriptions}: {}", output.stderr_text());
    }
    Ok(())
}

/// Delete several filter rules in one process.
///
/// iptables has no batch mode, so removing the anti-spoofing rules from two
/// chains took two processes on the workspace stop path. iptables-restore with
/// --noflush applies a whole table's worth of rules in one process, which is the
/// same work for half the spawns.
///
/// Unlike an insert, a delete is rejected when the rule is not there, and the
/// whole script is abandoned at the first rejected line: the lines after it never
/// run. That makes this safe only for rules a listing has just proved exist, and
/// the caller falls back to one delete per rule when the batch is rejected, which
/// is what covers a rule that disappeared between the listing and the delete.
pub(in crate::network) fn delete_filter_rules(
    iptables: &str,
    rules: &[(&str, &[&str])],
) -> Result<()> {
    if rules.is_empty() {
        return Ok(());
    }

    let mut script = String::from("*filter\n");
    for (chain, rule_args) in rules {
        script.push_str("-D ");
        script.push_str(chain);
        for argument in rule_args.iter() {
            script.push(' ');
            script.push_str(&quote_restore_argument(argument));
        }
        script.push('\n');
    }
    script.push_str("COMMIT\n");

    let restore = restore_binary_for(iptables);
    let descriptions = rules
        .iter()
        .map(|(chain, _)| *chain)
        .collect::<Vec<_>>()
        .join(", ");
    let output = HostCommand::new(&restore)
        .arg("--noflush")
        .stdin(script.into_bytes())
        .run()
        .with_context(|| format!("failed to delete {descriptions} rules via {restore}"))?;
    if !output.success() {
        bail!(
            "failed to delete {descriptions} rules: {}",
            output.stderr_text()
        );
    }
    Ok(())
}

/// Quote one argument for iptables-restore input.
///
/// The restore input is the format iptables-save writes, where a value containing
/// whitespace or quotes is wrapped in double quotes. The arguments come from
/// Enclave's own rule builders, so the only realistic case is a comment containing
/// spaces, but quoting everything that is not plainly safe keeps the parser from
/// splitting a value.
pub(in crate::network) fn quote_restore_argument(argument: &str) -> String {
    let plain = !argument.is_empty()
        && argument
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./:!=".contains(&byte));
    if plain {
        return argument.to_string();
    }
    let escaped = argument.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// The restore binary that matches the detected iptables.
///
/// The nft and legacy variants keep separate rule sets, so restoring through the
/// wrong one would add rules the running firewall never sees.
pub(in crate::network) fn restore_binary_for(iptables: &str) -> String {
    if iptables == "iptables" {
        "iptables-restore".to_string()
    } else {
        format!("{iptables}-restore")
    }
}
