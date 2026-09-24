use super::*;

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
pub(in crate::network) fn split_rule_args(rule: &str) -> Vec<String> {
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

pub(in crate::network) fn ensure_input_rule_first(
    iptables: &str,
    rule_args: &[&str],
    rule_desc: &str,
) -> Result<()> {
    ensure_rule(iptables, "filter", "INPUT", rule_args, true, rule_desc)
}

pub(in crate::network) fn ensure_forward_rule_first(
    iptables: &str,
    rule_args: &[&str],
    rule_desc: &str,
) -> Result<()> {
    ensure_rule(iptables, "filter", "FORWARD", rule_args, true, rule_desc)
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

    let check = Command::new(iptables)
        .args(&check_args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
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
    let output = Command::new(iptables)
        .args(&add_args)
        .output()
        .with_context(|| format!("failed to add {rule_desc} via {iptables}"))?;
    if !output.status.success() && !is_rule_already_exists_error(&output.stderr) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to add {rule_desc} ({}): {}",
            output.status,
            stderr.trim()
        );
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
    let output = Command::new(iptables)
        .args(&delete_args)
        .output()
        .with_context(|| format!("failed to remove {chain} rule via {iptables}"))?;
    if !output.status.success() && !is_rule_missing_error(&output.stderr) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to remove {chain} rule in table {table} ({}): {}",
            output.status,
            stderr.trim()
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
        let status = Command::new(candidate)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if let Ok(s) = status {
            if s.success() {
                return Ok((*candidate).to_string());
            }
        }
    }
    bail!(
        "no iptables binary found; install iptables, iptables-nft, or iptables-legacy \
         for workspace outbound networking"
    )
}
