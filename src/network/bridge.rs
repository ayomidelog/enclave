use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::hostcmd::HostCommand;

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

    if !bridge_members()?.is_empty() {
        return Ok(false);
    }

    run_ip(&["link", "set", BRIDGE_NAME, "down"])?;
    run_ip(&["link", "delete", BRIDGE_NAME, "type", "bridge"])?;
    Ok(true)
}

/// Whether the Enclave bridge interface exists.
///
/// This reads sysfs rather than running ip link show, because the daemon
/// re-checks the bridge on every workspace start. The two agree by construction
/// — both answer "is there a network interface with this name" — and the sysfs
/// lookup costs no process.
fn bridge_exists() -> Result<bool> {
    Ok(interface_path(BRIDGE_NAME).exists())
}

/// Names of the interfaces enslaved to the Enclave bridge.
///
/// A bridge exposes one directory per member under its brif sysfs entry, which
/// is the same list ip link show master prints.
fn bridge_members() -> Result<Vec<String>> {
    let directory = interface_path(BRIDGE_NAME).join("brif");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to list bridge members in {}", directory.display())
            })
        }
    };
    let mut members = Vec::new();
    for entry in entries {
        members.push(entry?.file_name().to_string_lossy().into_owned());
    }
    members.sort();
    Ok(members)
}

fn interface_path(interface: &str) -> PathBuf {
    Path::new("/sys/class/net").join(interface)
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
    let output = HostCommand::new("ip")
        .args(["addr", "show", "dev", BRIDGE_NAME])
        .run()
        .context("failed to inspect bridge address")?;
    let stdout = output.stdout_text();
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
/// Other interfaces on the host that already use the Enclave subnet.
///
/// The subnet and the bridge name are fixed rather than configurable, and this is
/// the half of that decision that matters. A host whose routes or interfaces already
/// use 10.200.0.0/24 is refused with the interfaces named, before any host state is
/// changed, which is the outcome the plan asks for: its completion criterion is that
/// startup refuses safely or chooses a non-conflicting configured network, and
/// refusing safely is the half that does not put a second unknown network on the
/// operator's host.
///
/// Making the subnet configurable is the other half and was not done. The subnet is
/// compiled into the shapes of the firewall rules Enclave compares against
/// `iptables -S` to prove ownership, so a configurable value has to reach every one
/// of those comparisons exactly or ownership detection starts mis-attributing rules
/// in both directions: Enclave's own rules read as foreign, and foreign rules read as
/// Enclave's. That is the mechanism the security boundary rests on, and a change to
/// it is not worth making for a host configuration this refuses clearly.
pub fn subnet_conflicts() -> Result<Vec<String>> {
    let output = HostCommand::new("ip")
        .args(["-o", "-4", "addr", "show"])
        .run_checked()
        .context("failed to list host IPv4 addresses")?;
    Ok(parse_subnet_conflicts(&output.stdout_text()))
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
    HostCommand::new("ip")
        .args(args)
        .run_checked()
        .with_context(|| format!("failed to run: ip {}", args.join(" ")))?;
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
