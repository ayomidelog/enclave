//! Talking to iptables: the argument format and the operations over it.
//!
//! Every call in this project goes through one of three shapes. A single rule is added
//! or deleted with one process. Several rules are applied with one `iptables-restore`
//! call, because the start and stop paths pay per process and the restore applies a
//! whole table at once. And the binary itself has to be detected once, because the nft
//! and legacy variants keep separate rule sets and restoring through the wrong one adds
//! rules the running firewall never sees.
//!
//! The split is by which of those three a function belongs to. The argument format is
//! here, because all three share it.

mod batch;
mod rules;

use super::*;

use crate::hostcmd::HostCommand;

use std::time::Duration;

#[cfg(test)]
pub(in crate::network) use batch::quote_restore_argument;
pub(in crate::network) use batch::{delete_filter_rules, insert_filter_rules_first};
// The restore binary name is what the nat tests assert, so it is reachable from the
// module root rather than only from the batch module that uses it.
#[cfg(test)]
pub(in crate::network) use batch::restore_binary_for;
// Only what is used outside this module tree is re-exported. The predicates that
// recognise a duplicate and a missing rule, and the probe they are built on, are used
// by the operations in the sibling module and nowhere else, so they stay private here
// rather than widening the surface every caller sees.
pub(in crate::network) use rules::{delete_rule, detect_iptables, ensure_rule, remove_filter_rule};

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
pub(crate) fn split_rule_args(rule: &str) -> Vec<String> {
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
