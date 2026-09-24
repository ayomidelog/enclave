use std::fs;

use anyhow::{bail, Context, Result};

use super::bridge::BRIDGE_NAME;
use super::ipam;

const METADATA_IPV4_CIDR: &str = "169.254.169.254/32";
const SYSCTL_IP_FORWARD: &str = "/proc/sys/net/ipv4/ip_forward";
const COMMENT_MODULE: &str = "comment";

/// Every workspace-scoped rule carries this prefix so repair and diagnostics
/// can tell Enclave-owned rules apart from unrelated host firewall state.
pub(crate) const RULE_COMMENT_PREFIX: &str = "enclave:";

/// Owner label for the rules that describe the bridge itself rather than one
/// workspace.
///
/// Giving every shared rule the same label lets `ensure_nat` retire the shapes a
/// previous release installed, and lets `doctor` inventory them.
pub(crate) const BRIDGE_RULE_OWNER: &str = "bridge";

/// Tables Enclave installs rules in. The shared bridge rules span both.
pub(crate) const RULE_TABLES: &[&str] = &["filter", "nat"];

/// Cap for one firewall dump. A host with many rules produces a large listing,
/// and the inventory only needs enough of it to identify Enclave's own rules.
pub(crate) const RULE_DUMP_CAP: usize = 8 * 1024 * 1024;

mod anti_spoof;
mod forwarding;
mod inventory;
mod primitives;

pub(crate) use anti_spoof::anti_spoof_chains_for;
pub(crate) use anti_spoof::{ensure_workspace_anti_spoofing, remove_workspace_anti_spoofing};
pub use forwarding::{ensure_nat, remove_nat};
pub(crate) use inventory::{list_owned_rules, remove_owned_rule, OwnedRule};
pub(crate) use primitives::split_rule_args;
#[cfg(test)]
pub(in crate::network) use primitives::{quote_restore_argument, restore_binary_for};

// The nat tests exercise the rule builders, the parser, and iptables discovery
// directly, so they reach the submodule internals through the module root.
#[cfg(test)]
pub(in crate::network) use anti_spoof::{anti_spoof_rule_args, chains_with_anti_spoof_rule};
#[cfg(test)]
pub(in crate::network) use forwarding::{bridge_rules, stale_bridge_rules};
#[cfg(test)]
pub(in crate::network) use inventory::parse_owned_rules;
#[cfg(test)]
pub(in crate::network) use primitives::detect_iptables;

#[cfg(test)]
#[path = "../../../tests/src/network/nat.rs"]
mod tests;
