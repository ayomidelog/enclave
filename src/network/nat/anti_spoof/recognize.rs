//! Identifying the anti-spoofing rule that belongs to one workspace interface.
//!
//! A rule is identified by what it matches rather than by the argument order it was
//! installed with, because iptables normalizes the order and a comparison against the
//! installed shape would match nothing. That is what makes both the removal and its
//! absence check non-vacuous, so the matching lives in one place.
//!
//! The dump helpers are here too: every question about the rule is answered from one
//! `iptables -S` listing, and reading the listing is the expensive part of a
//! stop on a host with a large table.

use super::primitives::split_rule_args;
use super::*;
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

/// Chains and rule bodies that carry an anti-spoofing rule for this interface
/// and address.
///
/// The body is the text after `-A <chain> `, which is exactly what `iptables -D`
/// wants back, so removal deletes the rule that is really there instead of
/// reconstructing the shape this release happens to install.
pub(in crate::network) fn anti_spoof_rules(
    dump: &str,
    veth_host: &str,
    assigned_ip: &str,
) -> Vec<(&'static str, String)> {
    let mut found = Vec::new();
    for chain in ["INPUT", "FORWARD"] {
        for line in dump.lines() {
            let Some(rule) = line.strip_prefix(&format!("-A {chain} ")) else {
                continue;
            };
            if is_anti_spoof_rule(rule, veth_host, assigned_ip) {
                found.push((chain, rule.to_string()));
            }
        }
    }
    found
}

pub(in crate::network) fn chains_with_anti_spoof_rule(
    dump: &str,
    veth_host: &str,
    assigned_ip: &str,
) -> Vec<&'static str> {
    let mut chains: Vec<&'static str> = Vec::new();
    for (chain, _) in anti_spoof_rules(dump, veth_host, assigned_ip) {
        if !chains.contains(&chain) {
            chains.push(chain);
        }
    }
    chains
}

/// Whether one `iptables -S` rule body is the anti-spoofing rule for this
/// interface and address.
///
/// The rule is identified by what it matches rather than by the argument order it
/// was installed with. `iptables` normalizes a negated match to the front of the
/// rule, so a rule added as `-i <veth> ! -s <ip>/32` is printed as
/// `! -s <ip>/32 -i <veth>`. Comparing against the installed form would match
/// nothing, which makes both the removal and its absence check vacuous.
pub(in crate::network) fn is_anti_spoof_rule(
    rule: &str,
    veth_host: &str,
    assigned_ip: &str,
) -> bool {
    let args = split_rule_args(rule);
    let target = args
        .windows(2)
        .any(|pair| pair[0] == "-j" && pair[1] == "DROP");
    if !target {
        return false;
    }
    let interface_matches = args
        .windows(2)
        .any(|pair| pair[0] == "-i" && pair[1] == veth_host);
    let negated_source_matches = args.windows(3).any(|triple| {
        triple[0] == "!" && triple[1] == "-s" && triple[2] == format!("{assigned_ip}/32")
    });
    interface_matches && negated_source_matches
}

pub(in crate::network) fn run_iptables_dump(iptables: &str) -> Result<String> {
    let output = HostCommand::new(iptables)
        .arg("-S")
        .output_cap(RULE_DUMP_CAP)
        .run_checked()
        .with_context(|| format!("failed to list firewall rules via {iptables}"))?;
    Ok(output.stdout_text())
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
