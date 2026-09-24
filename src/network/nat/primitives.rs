use super::*;

pub(in crate::network) fn ensure_forward_rule(
    iptables: &str,
    rule_args: &[&str],
    rule_desc: &str,
) -> Result<()> {
    ensure_filter_rule(iptables, "FORWARD", rule_args, false, rule_desc)
}

pub(in crate::network) fn ensure_input_rule_first(
    iptables: &str,
    rule_args: &[&str],
    rule_desc: &str,
) -> Result<()> {
    ensure_filter_rule(iptables, "INPUT", rule_args, true, rule_desc)
}

pub(in crate::network) fn ensure_forward_rule_first(
    iptables: &str,
    rule_args: &[&str],
    rule_desc: &str,
) -> Result<()> {
    ensure_filter_rule(iptables, "FORWARD", rule_args, true, rule_desc)
}

pub(in crate::network) fn ensure_filter_rule(
    iptables: &str,
    chain: &str,
    rule_args: &[&str],
    insert_first: bool,
    rule_desc: &str,
) -> Result<()> {
    let mut check_args = vec!["-C", chain];
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
        vec!["-I", chain, "1"]
    } else {
        vec!["-A", chain]
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

pub(in crate::network) fn remove_forward_rule(iptables: &str, rule_args: &[&str]) -> Result<()> {
    remove_filter_rule(iptables, "FORWARD", rule_args)
}

pub(in crate::network) fn remove_input_rule(iptables: &str, rule_args: &[&str]) -> Result<()> {
    remove_filter_rule(iptables, "INPUT", rule_args)
}

pub(in crate::network) fn remove_filter_rule(
    iptables: &str,
    chain: &str,
    rule_args: &[&str],
) -> Result<()> {
    let mut delete_args = vec!["-D", chain];
    delete_args.extend_from_slice(rule_args);
    let output = Command::new(iptables)
        .args(&delete_args)
        .output()
        .with_context(|| format!("failed to remove {chain} rule via {iptables}"))?;
    if !output.status.success() && !is_rule_missing_error(&output.stderr) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to remove {} rule for {} ({}): {}",
            chain,
            ipam::SUBNET_CIDR,
            output.status,
            stderr.trim()
        );
    }
    Ok(())
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
