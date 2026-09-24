use super::primitives::{
    detect_iptables, ensure_forward_rule, ensure_forward_rule_first, ensure_input_rule_first,
    is_rule_already_exists_error, is_rule_missing_error, remove_forward_rule, remove_input_rule,
};
use super::*;

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

pub(in crate::network) fn ensure_forward_rules() -> Result<()> {
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

pub(in crate::network) fn remove_forward_rules(iptables: &str) -> Result<()> {
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
    );
}

pub(in crate::network) fn ensure_masquerade() -> Result<()> {
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
