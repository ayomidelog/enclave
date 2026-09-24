use super::primitives::{
    detect_iptables, ensure_forward_rule_first, ensure_input_rule_first, remove_filter_rule,
};
use super::*;

pub fn ensure_workspace_anti_spoofing(
    veth_host: &str,
    assigned_ip: &str,
    workspace_id: &str,
) -> Result<()> {
    let iptables = detect_iptables()?;
    // One `iptables -S` tells us which chains already drop spoofed sources for
    // this interface, so a restart does not pay a probe per chain.
    let present = anti_spoof_chains_present(&iptables, veth_host, assigned_ip)?;
    let rule = anti_spoof_rule_args(veth_host, assigned_ip, Some(workspace_id));
    let rule_refs: Vec<&str> = rule.iter().map(String::as_str).collect();

    if !present.contains(&"INPUT") {
        ensure_input_rule_first(
            &iptables,
            &rule_refs,
            "block spoofed source addresses from workspace interface to host",
        )?;
    }
    if !present.contains(&"FORWARD") {
        ensure_forward_rule_first(
            &iptables,
            &rule_refs,
            "block spoofed source addresses from workspace interface to forwarded destinations",
        )?;
    }
    Ok(())
}

pub fn remove_workspace_anti_spoofing(
    veth_host: &str,
    assigned_ip: &str,
    workspace_id: &str,
) -> Result<()> {
    let iptables = detect_iptables()?;
    // One `iptables -S` lists the whole filter table, which is enough to learn
    // which chains still carry this interface's rule. Checking per rule shape
    // cost one process per candidate and dominated workspace stop time.
    let present = anti_spoof_chains_present(&iptables, veth_host, assigned_ip)?;
    for chain in present {
        // Prefer the tagged shape this version installs, then fall back to the
        // untagged shape older releases used.
        let tagged = anti_spoof_rule_args(veth_host, assigned_ip, Some(workspace_id));
        let tagged_refs: Vec<&str> = tagged.iter().map(String::as_str).collect();
        let legacy = anti_spoof_rule_args(veth_host, assigned_ip, None);
        let legacy_refs: Vec<&str> = legacy.iter().map(String::as_str).collect();
        remove_filter_rule(&iptables, chain, &tagged_refs)
            .or_else(|_| remove_filter_rule(&iptables, chain, &legacy_refs))?;
    }
    verify_anti_spoofing_absent(&iptables, veth_host, assigned_ip)
}

/// Chains in the filter table that still carry an anti-spoofing rule for this
/// interface and address, whatever comment shape installed it.
pub(in crate::network) fn anti_spoof_chains_present(
    iptables: &str,
    veth_host: &str,
    assigned_ip: &str,
) -> Result<Vec<&'static str>> {
    let dump = run_iptables_dump(iptables)?;
    Ok(chains_with_anti_spoof_rule(&dump, veth_host, assigned_ip))
}

pub(in crate::network) fn chains_with_anti_spoof_rule(
    dump: &str,
    veth_host: &str,
    assigned_ip: &str,
) -> Vec<&'static str> {
    let signature = anti_spoof_signature(veth_host, assigned_ip);
    ["INPUT", "FORWARD"]
        .into_iter()
        .filter(|chain| {
            dump.lines()
                .any(|line| line.starts_with(&format!("-A {chain} ")) && line.contains(&signature))
        })
        .collect()
}

/// The interface and source pattern that identifies an Enclave anti-spoofing
/// rule regardless of which comment or version installed it.
pub(in crate::network) fn anti_spoof_signature(veth_host: &str, assigned_ip: &str) -> String {
    format!("-i {veth_host} ! -s {assigned_ip}/32")
}

pub(in crate::network) fn run_iptables_dump(iptables: &str) -> Result<String> {
    let output = Command::new(iptables)
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
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Re-check the firewall after deletion. `iptables -D` reports success even
/// when another copy of the rule survives, so success must be proven by
/// absence rather than by exit status.
pub(in crate::network) fn verify_anti_spoofing_absent(
    iptables: &str,
    veth_host: &str,
    assigned_ip: &str,
) -> Result<()> {
    let remaining = anti_spoof_chains_present(iptables, veth_host, assigned_ip)?;
    if !remaining.is_empty() {
        bail!(
            "anti-spoofing rule for {} still exists after removal in chain(s): {}",
            veth_host,
            remaining.join(", ")
        );
    }
    Ok(())
}

pub(in crate::network) fn anti_spoof_rule_args(
    veth_host: &str,
    assigned_ip: &str,
    owner: Option<&str>,
) -> Vec<String> {
    let mut rule = vec![
        "-i".to_string(),
        veth_host.to_string(),
        "!".to_string(),
        "-s".to_string(),
        format!("{assigned_ip}/32"),
    ];
    if let Some(owner) = owner {
        rule.extend(comment_args(owner));
    }
    rule.push("-j".to_string());
    rule.push("DROP".to_string());
    rule
}

pub(in crate::network) fn comment_args(owner: &str) -> Vec<String> {
    vec![
        "-m".to_string(),
        COMMENT_MODULE.to_string(),
        "--comment".to_string(),
        format!("{RULE_COMMENT_PREFIX}{owner}"),
    ]
}
