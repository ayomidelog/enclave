//! Installing and removing a workspace interface anti-spoofing rules.
//!
//! One rule per chain drops packets whose source is not the workspace own address. The
//! rules are installed in one `iptables-restore` call, because the start path
//! pays per process, and removed from the listing the firewall returns rather than from
//! a shape this release reconstructs, because that is the only way to delete both the
//! tagged shape this release installs and the untagged one older releases left.
//!
//! The recognition half lives beside this one; everything here is about changing the
//! table rather than about reading it.

mod recognize;

// The nat tests exercise the rule builder and the recognizers directly, so they reach
// the recognition half through this module rather than through the submodule path.
#[cfg(test)]
pub(in crate::network) use recognize::{
    anti_spoof_rule_args, anti_spoof_rules, chains_with_anti_spoof_rule,
};

use recognize::{anti_spoof_chains_present, run_iptables_dump, verify_anti_spoofing_absent};

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
    let rule = recognize::anti_spoof_rule_args(veth_host, assigned_ip, Some(workspace_id));
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
/// left. The listing is read once to learn what to delete and once to prove the
/// deletion; both reads are counted in the metrics because they dominate a stop
/// on a host where a table dump costs tens of milliseconds.
///
/// The deletes themselves go in one process. A rule that vanishes between the
/// listing and the batch is not a problem: the batch is rejected as a whole, and
/// the per-rule fallback then finds nothing left to delete for it and removes the
/// others.
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
    remove_listed_anti_spoof_rules(&iptables, &dump, veth_host, assigned_ip)?;
    let verify_timer = crate::perf::Timer::new("network.rules.verify");
    let verified = verify_anti_spoofing_absent(&iptables, veth_host, assigned_ip);
    drop(verify_timer);
    verified
}

/// Delete the listed rules in one process, falling back to one at a time.
///
/// The batch is the common case: the listing proved every rule exists, so the
/// script has nothing to reject. The fallback exists because a delete can be
/// rejected for a rule that disappeared between the listing and the batch, and a
/// rejected batch abandons the lines after it, so one vanished rule would
/// otherwise leave the others in place.
fn remove_listed_anti_spoof_rules(
    iptables: &str,
    dump: &str,
    veth_host: &str,
    assigned_ip: &str,
) -> Result<()> {
    let listed = recognize::anti_spoof_rules(dump, veth_host, assigned_ip);
    if listed.is_empty() {
        return Ok(());
    }
    let owned = listed
        .iter()
        .map(|(chain, rule)| {
            let args = split_rule_args(rule);
            (*chain, args)
        })
        .collect::<Vec<_>>();
    let batch = owned
        .iter()
        .map(|(chain, args)| {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            (*chain, refs)
        })
        .collect::<Vec<_>>();
    let batch_refs = batch
        .iter()
        .map(|(chain, args)| (*chain, args.as_slice()))
        .collect::<Vec<_>>();
    if super::primitives::delete_filter_rules(iptables, &batch_refs).is_ok() {
        return Ok(());
    }

    tracing::debug!(
        "batched anti-spoofing rule deletion for {veth_host} was rejected; deleting rule by rule"
    );
    for (chain, args) in &owned {
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        remove_filter_rule_with_retry(iptables, chain, &refs)?;
    }
    Ok(())
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
