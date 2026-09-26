use std::fs;

use anyhow::{bail, Result};

use crate::network::nat::{self, OwnedRule};

use super::DoctorCheck;

/// Firewall rules Enclave owns that name an interface which no longer exists.
///
/// A rule is scoped to an interface, so once that interface is gone the rule can
/// never match anything again. It is provably Enclave's, because the ownership
/// comment is what put it in this list, so it is safe to remove without touching
/// any rule the host or another tool installed.
///
/// The shared bridge rules are excluded: they are scoped to the bridge and to the
/// subnet rather than to one workspace interface, and `ensure_nat` already retires
/// the shapes it does not install.
pub(crate) fn stale_anti_spoof_rules(rules: &[OwnedRule]) -> Vec<&OwnedRule> {
    rules
        .iter()
        .filter(|rule| rule.owner != nat::BRIDGE_RULE_OWNER)
        .filter(|rule| {
            let Some(interface) = rule_interface(&rule.rule) else {
                return false;
            };
            !interface_exists(&interface)
        })
        .collect()
}

/// The interface a rule is scoped to, when it names exactly one.
fn rule_interface(rule: &str) -> Option<String> {
    let args = nat::split_rule_args(rule);
    let mut interfaces = args
        .windows(2)
        .filter(|pair| pair[0] == "-i")
        .map(|pair| pair[1].clone());
    let interface = interfaces.next()?;
    interfaces.next().is_none().then_some(interface)
}

fn interface_exists(interface: &str) -> bool {
    fs::symlink_metadata(format!("/sys/class/net/{interface}")).is_ok()
}

/// Report Enclave-owned rules whose interface is gone.
pub(crate) fn check_stale_firewall_rules() -> DoctorCheck {
    const NAME: &str = "stale_firewall_rules";

    let rules = match nat::list_owned_rules() {
        Ok(rules) => rules,
        Err(err) => {
            return DoctorCheck::warn(NAME, &format!("firewall rule inventory unknown: {err:#}"))
        }
    };
    let stale = stale_anti_spoof_rules(&rules);
    if stale.is_empty() {
        return DoctorCheck::ok(
            NAME,
            &format!(
                "all {} Enclave-owned firewall rule(s) name an interface that exists",
                rules.len()
            ),
        );
    }
    let details = stale
        .iter()
        .map(|rule| format!("{} {} (owner {})", rule.table, rule.chain, rule.owner))
        .collect::<Vec<_>>()
        .join("; ");
    DoctorCheck::warn(
        NAME,
        &format!(
            "{} Enclave-owned firewall rule(s) name an interface that no longer exists: {details}",
            stale.len()
        ),
    )
}

/// Remove Enclave-owned rules whose interface is gone. Returns how many were
/// removed.
pub(crate) fn remove_stale_firewall_rules() -> Result<usize> {
    let rules = nat::list_owned_rules()?;
    let stale = stale_anti_spoof_rules(&rules);
    let mut removed = 0;
    let mut errors = Vec::new();
    for rule in stale {
        match nat::remove_owned_rule(rule) {
            Ok(()) => removed += 1,
            Err(error) => errors.push(format!(
                "{} {} (owner {}): {error:#}",
                rule.table, rule.chain, rule.owner
            )),
        }
    }
    if !errors.is_empty() {
        bail!(
            "failed to remove {} stale Enclave firewall rule(s): {}",
            errors.len(),
            errors.join("; ")
        );
    }
    Ok(removed)
}
