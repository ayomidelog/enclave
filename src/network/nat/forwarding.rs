use super::inventory::{list_owned_rules_across_tables, OwnedRule};
use super::primitives::{comment_args, delete_rule, detect_iptables, ensure_rule, split_rule_args};
use super::*;

/// One shared rule Enclave keeps in a chain for the bridge itself.
///
/// The rule shapes are part of the binary, so an upgrade can change them. Both
/// shapes are recorded: the tagged one this release installs, and the untagged
/// one older releases left behind, which must be retired rather than duplicated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::network) struct BridgeRule {
    pub(in crate::network) table: &'static str,
    pub(in crate::network) chain: &'static str,
    pub(in crate::network) insert_first: bool,
    pub(in crate::network) args: Vec<String>,
    pub(in crate::network) legacy_args: Vec<String>,
    pub(in crate::network) description: &'static str,
}

/// One shared rule as this release describes it.
struct BridgeRuleSpec {
    table: &'static str,
    chain: &'static str,
    insert_first: bool,
    /// The match half, in the order `iptables` should store it.
    matches: &'static [&'static str],
    target: &'static str,
    description: &'static str,
}

/// The shared rules the workspace bridge needs in order to route traffic.
///
/// Each spec is the match half plus the jump target. `iptables -S` prints the
/// matches in the order they were given and always places the target last, so
/// building the expected shape the same way keeps it comparable with a dump.
pub(in crate::network) fn bridge_rules() -> Vec<BridgeRule> {
    let specs = [
        BridgeRuleSpec {
            table: "filter",
            chain: "INPUT",
            insert_first: true,
            matches: &["-i", BRIDGE_NAME, "-m", "addrtype", "--dst-type", "LOCAL"],
            target: "DROP",
            description: "block workspace access to host-local services",
        },
        BridgeRuleSpec {
            table: "filter",
            chain: "FORWARD",
            insert_first: true,
            matches: &["-i", BRIDGE_NAME, "-o", BRIDGE_NAME],
            target: "DROP",
            description: "block workspace-to-workspace forwarding on enclave bridge",
        },
        BridgeRuleSpec {
            table: "filter",
            chain: "FORWARD",
            insert_first: true,
            matches: &["-s", ipam::SUBNET_CIDR, "-d", METADATA_IPV4_CIDR],
            target: "DROP",
            description: "block workspace access to cloud metadata endpoint",
        },
        BridgeRuleSpec {
            table: "filter",
            chain: "FORWARD",
            insert_first: false,
            matches: &["-s", ipam::SUBNET_CIDR],
            target: "ACCEPT",
            description: "allow outbound forwarding from enclave subnet",
        },
        BridgeRuleSpec {
            table: "filter",
            chain: "FORWARD",
            insert_first: false,
            matches: &[
                "-d",
                ipam::SUBNET_CIDR,
                "-m",
                "conntrack",
                "--ctstate",
                "RELATED,ESTABLISHED",
            ],
            target: "ACCEPT",
            description: "allow established return traffic to enclave subnet",
        },
        BridgeRuleSpec {
            table: "nat",
            chain: "POSTROUTING",
            insert_first: false,
            matches: &["-s", ipam::SUBNET_CIDR],
            target: "MASQUERADE",
            description: "masquerade workspace traffic",
        },
    ];

    specs
        .into_iter()
        .map(|spec| {
            let matches = spec
                .matches
                .iter()
                .map(|argument| (*argument).to_string())
                .collect::<Vec<_>>();
            let mut legacy_args = matches.clone();
            legacy_args.push("-j".to_string());
            legacy_args.push(spec.target.to_string());
            let mut args = matches;
            args.extend(comment_args(BRIDGE_RULE_OWNER));
            args.push("-j".to_string());
            args.push(spec.target.to_string());
            BridgeRule {
                table: spec.table,
                chain: spec.chain,
                insert_first: spec.insert_first,
                args,
                legacy_args,
                description: spec.description,
            }
        })
        .collect()
}

pub fn ensure_nat() -> Result<()> {
    ensure_ipv4_forwarding()?;
    let iptables = detect_iptables()?;
    let rules = bridge_rules();

    // Retire the shapes a previous release left behind before installing this
    // release's, so an upgrade does not stack two generations of shared rules.
    let retired = retire_stale_bridge_rules(&iptables, &rules)?;
    if !retired.is_empty() {
        tracing::info!(
            "retired {} stale shared firewall rule(s): {}",
            retired.len(),
            retired.join("; ")
        );
    }
    for rule in &rules {
        let args = rule.args.iter().map(String::as_str).collect::<Vec<_>>();
        ensure_rule(
            &iptables,
            rule.table,
            rule.chain,
            &args,
            rule.insert_first,
            rule.description,
        )?;
        // The untagged shape older releases installed has the same match, so
        // leaving it in place would duplicate this rule. It goes second so the
        // chain never loses the match while the tagged copy is being added.
        remove_legacy_bridge_rule(&iptables, rule)?;
    }
    verify_bridge_rules(&iptables, &rules)
}

pub fn remove_nat() -> Result<()> {
    let iptables = detect_iptables()?;
    // Retire every tagged bridge rule whatever shape installed it, then the
    // untagged shapes older releases used.
    retire_stale_bridge_rules(&iptables, &[])?;
    for rule in bridge_rules() {
        remove_legacy_bridge_rule(&iptables, &rule)?;
    }
    Ok(())
}

pub(in crate::network) fn ensure_ipv4_forwarding() -> Result<()> {
    let current =
        fs::read_to_string(SYSCTL_IP_FORWARD).context("failed to read IPv4 forwarding state")?;
    if current.trim() == "1" {
        return Ok(());
    }
    bail!(
        "IPv4 forwarding is disabled ({} = {}). Enable it explicitly before starting \
         Enclave workspaces with networking (e.g. `sysctl -w net.ipv4.ip_forward=1`) \
         or add `net.ipv4.ip_forward = 1` to /etc/sysctl.conf.",
        SYSCTL_IP_FORWARD,
        current.trim()
    )
}

fn remove_legacy_bridge_rule(iptables: &str, rule: &BridgeRule) -> Result<()> {
    let args = rule
        .legacy_args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    delete_rule(iptables, rule.table, rule.chain, &args)
}

/// Remove tagged bridge rules that are not part of `rules`.
///
/// An empty set therefore retires every shared rule Enclave owns, which is what
/// a teardown wants.
fn retire_stale_bridge_rules(iptables: &str, rules: &[BridgeRule]) -> Result<Vec<String>> {
    let mut retired = Vec::new();
    for owned in stale_bridge_rules(rules, &list_owned_rules_across_tables(iptables)?) {
        let args = split_rule_args(&owned.rule);
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        delete_rule(iptables, &owned.table, &owned.chain, &args)?;
        retired.push(format!("{} {} {}", owned.table, owned.chain, owned.rule));
    }
    Ok(retired)
}

/// Tagged bridge rules that this release would not install.
pub(in crate::network) fn stale_bridge_rules(
    rules: &[BridgeRule],
    owned: &[OwnedRule],
) -> Vec<OwnedRule> {
    owned
        .iter()
        .filter(|rule| rule.owner == BRIDGE_RULE_OWNER)
        .filter(|rule| {
            let args = split_rule_args(&rule.rule);
            !rules.iter().any(|current| {
                current.table == rule.table && current.chain == rule.chain && current.args == args
            })
        })
        .cloned()
        .collect()
}

/// Prove the shared rules are in place instead of trusting the add calls.
fn verify_bridge_rules(iptables: &str, rules: &[BridgeRule]) -> Result<()> {
    let present = list_owned_rules_across_tables(iptables)?
        .iter()
        .map(|rule| {
            (
                rule.table.clone(),
                rule.chain.clone(),
                split_rule_args(&rule.rule),
            )
        })
        .collect::<Vec<_>>();
    let missing = rules
        .iter()
        .filter(|rule| {
            !present.iter().any(|(table, chain, args)| {
                table == rule.table && chain == rule.chain && args == &rule.args
            })
        })
        .map(|rule| format!("{} {} {}", rule.table, rule.chain, rule.description))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    bail!(
        "shared bridge firewall rules are incomplete: {}",
        missing.join("; ")
    )
}
