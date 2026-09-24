use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::ipam;

pub const BRIDGE_NAME: &str = "enclave0";

const SUBNET_PREFIX_LEN: &str = "24";

pub fn ensure_bridge() -> Result<()> {
    ensure_subnet_available()?;
    if bridge_exists()? {
        ensure_bridge_address()?;
        ensure_bridge_up()?;
        disable_ipv6(BRIDGE_NAME)?;
        return Ok(());
    }

    create_bridge()?;
    assign_bridge_address()?;
    ensure_bridge_up()?;
    disable_ipv6(BRIDGE_NAME)?;
    Ok(())
}

pub fn remove_bridge_if_idle() -> Result<bool> {
    if !bridge_exists()? {
        return Ok(true);
    }

    let output = Command::new("ip")
        .args(["link", "show", "master", BRIDGE_NAME])
        .output()
        .context("failed to list bridge members")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        return Ok(false);
    }

    run_ip(&["link", "set", BRIDGE_NAME, "down"])?;
    run_ip(&["link", "delete", BRIDGE_NAME, "type", "bridge"])?;
    Ok(true)
}

fn bridge_exists() -> Result<bool> {
    let status = Command::new("ip")
        .args(["link", "show", BRIDGE_NAME])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("failed to check bridge existence")?;
    Ok(status.success())
}

pub fn bridge_is_present() -> Result<bool> {
    bridge_exists()
}

fn create_bridge() -> Result<()> {
    run_ip(&["link", "add", BRIDGE_NAME, "type", "bridge"])
        .with_context(|| format!("failed to create bridge {BRIDGE_NAME}"))
}

fn assign_bridge_address() -> Result<()> {
    let addr = format!("{}/{}", ipam::GATEWAY_IP, SUBNET_PREFIX_LEN);
    run_ip(&["addr", "add", &addr, "dev", BRIDGE_NAME])
        .with_context(|| format!("failed to assign address to {BRIDGE_NAME}"))
}

fn ensure_bridge_up() -> Result<()> {
    run_ip(&["link", "set", BRIDGE_NAME, "up"])
        .with_context(|| format!("failed to bring up {BRIDGE_NAME}"))
}

fn ensure_bridge_address() -> Result<()> {
    let output = Command::new("ip")
        .args(["addr", "show", "dev", BRIDGE_NAME])
        .output()
        .context("failed to inspect bridge address")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = format!("{}/{}", ipam::GATEWAY_IP, SUBNET_PREFIX_LEN);
    let addresses = parse_ipv4_addresses(&stdout);
    if addresses.iter().any(|address| address == &expected) {
        return Ok(());
    }
    if addresses
        .iter()
        .any(|address| address.starts_with("10.200.0."))
    {
        bail!(
            "bridge {} has an incompatible Enclave subnet address; expected {} but found {}",
            BRIDGE_NAME,
            expected,
            addresses.join(", ")
        );
    }
    if !addresses.is_empty() {
        bail!(
            "bridge {} already exists with incompatible IPv4 address(es): {}",
            BRIDGE_NAME,
            addresses.join(", ")
        );
    }

    assign_bridge_address()
}

/// Interfaces outside Enclave that already hold an address inside the Enclave
/// subnet.
///
/// The bridge address is fixed at 10.200.0.1/24, so a second interface on the
/// same /24 gives the kernel two connected routes for one prefix. Workspace
/// traffic can then leave through, or be delivered to, a network Enclave does
/// not control, while every workspace still reports clean anti-spoofing rules.
pub fn subnet_conflicts() -> Result<Vec<String>> {
    let output = Command::new("ip")
        .args(["-o", "-4", "addr", "show"])
        .output()
        .context("failed to list host IPv4 addresses")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "listing host IPv4 addresses failed ({}): {}",
            output.status,
            stderr.trim()
        );
    }
    Ok(parse_subnet_conflicts(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn parse_subnet_conflicts(output: &str) -> Vec<String> {
    parse_ipv4_addresses_by_interface(output)
        .into_iter()
        .filter(|(interface, _)| interface != BRIDGE_NAME)
        .filter(|(_, address)| ipam::is_in_subnet(address))
        .map(|(interface, address)| format!("{interface} ({address})"))
        .collect()
}

/// Read `ip -o -4 addr show` output into `(interface, address/prefix)` pairs.
fn parse_ipv4_addresses_by_interface(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let index = fields.next()?;
            if !index.ends_with(':') {
                return None;
            }
            let interface = fields.next()?.to_string();
            while let Some(field) = fields.next() {
                if field == "inet" {
                    return fields
                        .next()
                        .map(|address| (interface, address.to_string()));
                }
            }
            None
        })
        .collect()
}

/// Refuse to build the Enclave subnet on top of a host that already uses it.
fn ensure_subnet_available() -> Result<()> {
    let conflicts = subnet_conflicts()?;
    if conflicts.is_empty() {
        return Ok(());
    }
    bail!(
        "Enclave subnet {} is already in use by {}; remove that address or move Enclave to a \
         free subnet before starting workspaces with networking",
        ipam::SUBNET_CIDR,
        conflicts.join(", ")
    )
}

fn parse_ipv4_addresses(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            while let Some(field) = fields.next() {
                if field == "inet" {
                    return fields.next().map(str::to_string);
                }
            }
            None
        })
        .collect()
}

fn run_ip(args: &[&str]) -> Result<()> {
    let output = Command::new("ip")
        .args(args)
        .output()
        .with_context(|| format!("failed to run: ip {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "ip {} failed ({}): {}",
            args.join(" "),
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

pub fn disable_ipv6(interface: &str) -> Result<()> {
    let path = Path::new("/proc/sys/net/ipv6/conf")
        .join(interface)
        .join("disable_ipv6");
    if !path.exists() {
        return Ok(());
    }
    fs::write(&path, b"1")
        .with_context(|| format!("failed to disable IPv6 on interface {}", interface))?;
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/src/network/bridge.rs"]
mod tests;
