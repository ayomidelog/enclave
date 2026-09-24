use super::primitives::{
    comment_args, detect_iptables, insert_filter_rule_first, remove_filter_rule, split_rule_args,
};
use super::*;

use crate::hostcmd::HostCommand;

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
        insert_filter_rule_first(
            &iptables,
            "INPUT",
            &rule_refs,
            "block spoofed source addresses from workspace interface to host",
        )?;
    }
    if !present.contains(&"FORWARD") {
        insert_filter_rule_first(
            &iptables,
            "FORWARD",
            &rule_refs,
            "block spoofed source addresses from workspace interface to forwarded destinations",
        )?;
    }
    Ok(())
}

/// Whether the anti-spoofing rule for this interface is still installed.
///
/// Used by destroy verification, which has to prove the rule is gone rather than
/// trust that the removal ran.
pub(crate) fn anti_spoof_chains_for(
    veth_host: &str,
    assigned_ip: &str,
) -> Result<Vec<&'static str>> {
    let iptables = detect_iptables()?;
    anti_spoof_chains_present(&iptables, veth_host, assigned_ip)
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
    ["INPUT", "FORWARD"]
        .into_iter()
        .filter(|chain| {
            dump.lines().any(|line| {
                let Some(rule) = line.strip_prefix(&format!("-A {chain} ")) else {
                    return false;
                };
                is_anti_spoof_rule(rule, veth_host, assigned_ip)
            })
        })
        .collect()
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
