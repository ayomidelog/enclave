//! One rule at a time, and finding the binary to do it with.
//!
//! The error classification is here rather than at the call sites because the wording
//! is the whole signal: iptables reports a duplicate rule and a missing rule as text,
//! and only those two are recoverable, so the two predicates that recognise them are
//! what make a retry or a fallback possible at all.

use super::*;

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
