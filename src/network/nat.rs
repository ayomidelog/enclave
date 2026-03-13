use std::fs;
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::bridge::BRIDGE_NAME;
use super::ipam;

const METADATA_IPV4_CIDR: &str = "169.254.169.254/32";
const SYSCTL_IP_FORWARD: &str = "/proc/sys/net/ipv4/ip_forward";

pub fn ensure_nat() -> Result<()> {
    ensure_ipv4_forwarding()?;
    ensure_forward_rules()?;
    ensure_masquerade()?;
    Ok(())
}

pub fn remove_nat() -> Result<()> {
    let iptables = detect_iptables()?;
    remove_forward_rules(&iptables)?;
    let output = Command::new(&iptables)
        .args([
            "-t",
            "nat",
            "-D",
            "POSTROUTING",
            "-s",
            ipam::SUBNET_CIDR,
            "-j",
            "MASQUERADE",
        ])
        .output()
        .with_context(|| format!("failed to remove masquerade rule via {iptables}"))?;
    if !output.status.success() && !is_rule_missing_error(&output.stderr) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to remove MASQUERADE rule for {} ({}): {}",
            ipam::SUBNET_CIDR,
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

fn ensure_forward_rules() -> Result<()> {
    let iptables = detect_iptables()?;

    ensure_input_rule_first(
        &iptables,
        &[
            "-i",
            BRIDGE_NAME,
            "-m",
            "addrtype",
            "--dst-type",
            "LOCAL",
            "-j",
            "DROP",
        ],
        "block workspace access to host-local services",
    )?;

    ensure_forward_rule_first(
        &iptables,
        &["-i", BRIDGE_NAME, "-o", BRIDGE_NAME, "-j", "DROP"],
        "block workspace-to-workspace forwarding on enclave bridge",
    )?;

    ensure_forward_rule_first(
        &iptables,
        &[
            "-s",
            ipam::SUBNET_CIDR,
            "-d",
            METADATA_IPV4_CIDR,
            "-j",
            "DROP",
        ],
        "block workspace access to cloud metadata endpoint",
    )?;

    ensure_forward_rule(
        &iptables,
        &["-s", ipam::SUBNET_CIDR, "-j", "ACCEPT"],
        "allow outbound forwarding from enclave subnet",
    )?;

    ensure_forward_rule(
        &iptables,
        &[
            "-d",
            ipam::SUBNET_CIDR,
            "-m",
            "conntrack",
            "--ctstate",
            "RELATED,ESTABLISHED",
            "-j",
            "ACCEPT",
        ],
        "allow established return traffic to enclave subnet",
    )?;

    Ok(())
}

fn remove_forward_rules(iptables: &str) -> Result<()> {
    remove_forward_rule(
        iptables,
        &[
            "-d",
            ipam::SUBNET_CIDR,
            "-m",
            "conntrack",
            "--ctstate",
            "RELATED,ESTABLISHED",
            "-j",
            "ACCEPT",
        ],
    )?;
    remove_forward_rule(iptables, &["-s", ipam::SUBNET_CIDR, "-j", "ACCEPT"])?;
    remove_forward_rule(
        iptables,
        &[
            "-s",
            ipam::SUBNET_CIDR,
            "-d",
            METADATA_IPV4_CIDR,
            "-j",
            "DROP",
        ],
    )?;
    remove_forward_rule(
        iptables,
        &["-i", BRIDGE_NAME, "-o", BRIDGE_NAME, "-j", "DROP"],
    )?;
    remove_input_rule(
        iptables,
        &[
            "-i",
            BRIDGE_NAME,
            "-m",
            "addrtype",
            "--dst-type",
            "LOCAL",
            "-j",
            "DROP",
        ],
    )?;
    Ok(())
}

fn ensure_forward_rule(iptables: &str, rule_args: &[&str], rule_desc: &str) -> Result<()> {
    ensure_filter_rule(iptables, "FORWARD", rule_args, false, rule_desc)
}

fn ensure_input_rule_first(iptables: &str, rule_args: &[&str], rule_desc: &str) -> Result<()> {
    ensure_filter_rule(iptables, "INPUT", rule_args, true, rule_desc)
}

fn ensure_forward_rule_first(iptables: &str, rule_args: &[&str], rule_desc: &str) -> Result<()> {
    ensure_filter_rule(iptables, "FORWARD", rule_args, true, rule_desc)
}

fn ensure_filter_rule(
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

fn remove_forward_rule(iptables: &str, rule_args: &[&str]) -> Result<()> {
    remove_filter_rule(iptables, "FORWARD", rule_args)
}

fn remove_input_rule(iptables: &str, rule_args: &[&str]) -> Result<()> {
    remove_filter_rule(iptables, "INPUT", rule_args)
}

fn remove_filter_rule(iptables: &str, chain: &str, rule_args: &[&str]) -> Result<()> {
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

pub fn ensure_workspace_anti_spoofing(veth_host: &str, assigned_ip: &str) -> Result<()> {
    let iptables = detect_iptables()?;
    let rule = anti_spoof_rule_args(veth_host, assigned_ip);
    let rule_refs: Vec<&str> = rule.iter().map(String::as_str).collect();

    ensure_input_rule_first(
        &iptables,
        &rule_refs,
        "block spoofed source addresses from workspace interface to host",
    )?;
    ensure_forward_rule_first(
        &iptables,
        &rule_refs,
        "block spoofed source addresses from workspace interface to forwarded destinations",
    )?;
    Ok(())
}

pub fn remove_workspace_anti_spoofing(veth_host: &str, assigned_ip: &str) -> Result<()> {
    let iptables = detect_iptables()?;
    let rule = anti_spoof_rule_args(veth_host, assigned_ip);
    let rule_refs: Vec<&str> = rule.iter().map(String::as_str).collect();
    remove_input_rule(&iptables, &rule_refs)?;
    remove_forward_rule(&iptables, &rule_refs)?;
    Ok(())
}

fn anti_spoof_rule_args(veth_host: &str, assigned_ip: &str) -> Vec<String> {
    vec![
        "-i".to_string(),
        veth_host.to_string(),
        "!".to_string(),
        "-s".to_string(),
        format!("{assigned_ip}/32"),
        "-j".to_string(),
        "DROP".to_string(),
    ]
}

fn ensure_ipv4_forwarding() -> Result<()> {
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
    );
}

fn ensure_masquerade() -> Result<()> {
    let iptables = detect_iptables()?;

    let check = Command::new(&iptables)
        .args([
            "-t",
            "nat",
            "-C",
            "POSTROUTING",
            "-s",
            ipam::SUBNET_CIDR,
            "-j",
            "MASQUERADE",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .with_context(|| format!("failed to check masquerade rule via {iptables}"))?;

    if check.success() {
        return Ok(());
    }

    let output = Command::new(&iptables)
        .args([
            "-t",
            "nat",
            "-A",
            "POSTROUTING",
            "-s",
            ipam::SUBNET_CIDR,
            "-j",
            "MASQUERADE",
        ])
        .output()
        .with_context(|| format!("failed to add masquerade rule via {iptables}"))?;

    if !output.status.success() {
        if is_rule_already_exists_error(&output.stderr) {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to add MASQUERADE rule for {} ({}): {}",
            ipam::SUBNET_CIDR,
            output.status,
            stderr.trim()
        );
    }

    Ok(())
}

fn is_rule_already_exists_error(stderr: &[u8]) -> bool {
    let msg = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    msg.contains("rule already exists")
        || msg.contains("rule is duplicated")
        || (msg.contains("file exists") && msg.contains("rule in chain"))
}

fn is_rule_missing_error(stderr: &[u8]) -> bool {
    let msg = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    msg.contains("no chain/target/match by that name")
        || msg.contains("does a matching rule exist in that chain")
}

fn detect_iptables() -> Result<String> {
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

#[cfg(test)]
#[path = "../../tests/src/network/nat.rs"]
mod tests;
