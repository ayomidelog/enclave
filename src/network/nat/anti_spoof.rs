use super::primitives::{
    comment_args, detect_iptables, insert_filter_rules_first, remove_filter_rule, split_rule_args,
};
use super::*;

use crate::hostcmd::HostCommand;

use std::time::Duration;

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

    // The rules that are missing are installed in one iptables-restore call. Both
    // chains carry the same match, and the start path pays per process, so this is
    // the same rule set for one spawn instead of one per chain.
    let mut missing: Vec<(&str, &[&str], &str)> = Vec::new();
    if !present.contains(&"INPUT") {
        missing.push((
            "INPUT",
            &rule_refs,
            "block spoofed source addresses from workspace interface to host",
        ));
    }
    if !present.contains(&"FORWARD") {
        missing.push((
            "FORWARD",
            &rule_refs,
            "block spoofed source addresses from workspace interface to forwarded destinations",
        ));
    }
    insert_filter_rules_first(&iptables, &missing)?;
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

/// How many times the delete of one anti-spoofing rule is attempted.
const REMOVE_ATTEMPTS: usize = 3;

/// Remove this workspace's anti-spoofing rules and prove they are gone.
///
/// The rules are deleted from the listing the firewall returns rather than from a
/// shape this release reconstructs, because that is the only way to delete both
/// the tagged shape this release installs and the untagged one older releases
/// left. Batching the guess is not an option either: a delete-then-insert script
/// fed to iptables-restore --noflush is rejected whole when one delete matches
/// nothing, so the insert that follows never runs. The listing is therefore read
/// once to learn what to delete and once to prove the deletion, and both reads
/// are counted in the metrics because they dominate a stop on a host where a
/// table dump costs tens of milliseconds.
pub fn remove_workspace_anti_spoofing(veth_host: &str, assigned_ip: &str) -> Result<()> {
    let iptables = detect_iptables()?;
    // One `iptables -S` lists the whole filter table, and the rules it prints are
    // the ones that exist. Deleting from that listing removes the shape that is
    // really installed, tagged or legacy, instead of the shape this release
    // guesses. The guess matters because `iptables -D` reports success for a rule
    // that is not present, so a wrong guess looks like a successful removal and
    // only fails the absence check that follows.
    let dump_timer = crate::perf::Timer::new("network.rules.dump");
    let dump = run_iptables_dump(&iptables)?;
    drop(dump_timer);
    for (chain, rule) in anti_spoof_rules(&dump, veth_host, assigned_ip) {
        let args = split_rule_args(&rule);
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        remove_filter_rule_with_retry(&iptables, chain, &args)?;
    }
    let verify_timer = crate::perf::Timer::new("network.rules.verify");
    let verified = verify_anti_spoofing_absent(&iptables, veth_host, assigned_ip);
    drop(verify_timer);
    verified
}

/// Delete one rule, retrying a bounded number of times with backoff.
///
/// A delete can fail transiently while another writer holds the table lock, and
/// the absence check that follows treats a leftover rule as a failure, so a
/// short retry turns a flaky stop into a successful one. The attempt count and
/// the time the retries waited are both recorded, so a host that needs them is
/// visible in the metrics rather than only in a debug log.
fn remove_filter_rule_with_retry(iptables: &str, chain: &str, rule_args: &[&str]) -> Result<()> {
    let mut last_error = None;
    for attempt in 0..REMOVE_ATTEMPTS {
        match remove_filter_rule(iptables, chain, rule_args) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 == REMOVE_ATTEMPTS {
            break;
        }
        let delay = Duration::from_millis(20 * (attempt as u64 + 1));
        crate::perf::record_cleanup_retry();
        crate::perf::record_cleanup_retry_delay(delay.as_micros() as u64);
        tracing::debug!(
            "retrying removal of a {chain} rule via {iptables} after a transient error (attempt {}/{})",
            attempt + 1,
            REMOVE_ATTEMPTS
        );
        std::thread::sleep(delay);
    }
    match last_error {
        Some(error) => Err(error),
        None => bail!("failed to remove the {chain} rule for {iptables}"),
    }
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
