use super::*;

use crate::hostcmd::HostCommand;

use std::time::Duration;

/// Append the ownership comment that marks a rule as Enclave's.
pub(in crate::network) fn comment_args(owner: &str) -> Vec<String> {
    vec![
        "-m".to_string(),
        COMMENT_MODULE.to_string(),
        "--comment".to_string(),
        format!("{RULE_COMMENT_PREFIX}{owner}"),
    ]
}

/// Split one `iptables -S` rule body into the arguments `iptables -D` expects.
///
/// `iptables -S` quotes only the values that need it, and the ownership comment
/// is the one value Enclave reads back, so a double-quote-aware split is enough
/// to rebuild the argument list. A mistyped argument list cannot delete the
/// wrong rule: `iptables -D` only removes a rule that matches every argument.
pub(crate) fn split_rule_args(rule: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut pending = false;
    for character in rule.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                pending = true;
            }
            c if c.is_whitespace() && !quoted => {
                if pending {
                    args.push(std::mem::take(&mut current));
                    pending = false;
                }
            }
            c => {
                current.push(c);
                pending = true;
            }
        }
    }
    if pending {
        args.push(current);
    }
    args
}

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

/// Add a rule unless an identical one is already in the chain.
pub(in crate::network) fn ensure_rule(
    iptables: &str,
    table: &str,
    chain: &str,
    rule_args: &[&str],
    insert_first: bool,
    rule_desc: &str,
) -> Result<()> {
    let mut check_args = vec!["-t", table, "-C", chain];
    check_args.extend_from_slice(rule_args);

    // A check only reports through its exit status, so its output is discarded
    // and the call stays one spawn on the workspace start path.
    let check = HostCommand::new(iptables)
        .args(&check_args)
        .discard_output()
        .run()
        .with_context(|| format!("failed to check {rule_desc} via {iptables}"))?;
    if check.success() {
        return Ok(());
    }

    let mut add_args = if insert_first {
        vec!["-t", table, "-I", chain, "1"]
    } else {
        vec!["-t", table, "-A", chain]
    };
    add_args.extend_from_slice(rule_args);
    let output = HostCommand::new(iptables)
        .args(&add_args)
        .run()
        .with_context(|| format!("failed to add {rule_desc} via {iptables}"))?;
    if !output.success() && !is_rule_already_exists_error(&output.stderr) {
        bail!("failed to add {rule_desc}: {}", output.stderr_text());
    }
    Ok(())
}

/// Delete the first rule in `chain` that matches every argument.
pub(in crate::network) fn delete_rule(
    iptables: &str,
    table: &str,
    chain: &str,
    rule_args: &[&str],
) -> Result<()> {
    let mut delete_args = vec!["-t", table, "-D", chain];
    delete_args.extend_from_slice(rule_args);
    let output = HostCommand::new(iptables)
        .args(&delete_args)
        .run()
        .with_context(|| format!("failed to remove {chain} rule via {iptables}"))?;
    if !output.success() && !is_rule_missing_error(&output.stderr) {
        bail!(
            "failed to remove {chain} rule in table {table}: {}",
            output.stderr_text()
        );
    }
    Ok(())
}

pub(in crate::network) fn remove_filter_rule(
    iptables: &str,
    chain: &str,
    rule_args: &[&str],
) -> Result<()> {
    delete_rule(iptables, "filter", chain, rule_args)
}

pub(in crate::network) fn is_rule_already_exists_error(stderr: &[u8]) -> bool {
    let msg = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    msg.contains("rule already exists")
        || msg.contains("rule is duplicated")
        || (msg.contains("file exists") && msg.contains("rule in chain"))
}

pub(in crate::network) fn is_rule_missing_error(stderr: &[u8]) -> bool {
    let msg = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    msg.contains("no chain/target/match by that name")
        || msg.contains("does a matching rule exist in that chain")
}

pub(in crate::network) fn detect_iptables() -> Result<String> {
    static DETECTED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if let Some(binary) = DETECTED.get() {
        return Ok(binary.clone());
    }
    let binary = probe_iptables()?;
    // Only a successful probe is cached. A missing binary is re-probed so an
    // operator who installs iptables does not have to restart the daemon.
    let _ = DETECTED.set(binary.clone());
    Ok(binary)
}

pub(in crate::network) fn probe_iptables() -> Result<String> {
    for candidate in &["iptables-nft", "iptables-legacy", "iptables"] {
        let probe = HostCommand::new(candidate)
            .arg("--version")
            .discard_output()
            .timeout(Duration::from_secs(5))
            .run();
        if probe.is_ok_and(|output| output.success()) {
            return Ok((*candidate).to_string());
        }
    }
    bail!(
        "no iptables binary found; install iptables, iptables-nft, or iptables-legacy \
         for workspace outbound networking"
    )
}
