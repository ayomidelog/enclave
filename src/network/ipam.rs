use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use anyhow::{bail, Result};

pub const SUBNET_PREFIX: [u8; 3] = [10, 200, 0];
pub const GATEWAY_IP: &str = "10.200.0.1";
pub const SUBNET_CIDR: &str = "10.200.0.0/24";

const POOL_START: u8 = 10;

const POOL_END: u8 = 254;

pub fn allocate_ip(used: &BTreeSet<u8>) -> Result<String> {
    for octet in POOL_START..=POOL_END {
        if !used.contains(&octet) {
            return Ok(format_ip(octet));
        }
    }
    bail!(
        "IP address pool exhausted ({}.{}.{}.{}–{}.{}.{}.{})",
        SUBNET_PREFIX[0],
        SUBNET_PREFIX[1],
        SUBNET_PREFIX[2],
        POOL_START,
        SUBNET_PREFIX[0],
        SUBNET_PREFIX[1],
        SUBNET_PREFIX[2],
        POOL_END,
    )
}

pub fn parse_host_octet(ip: &str) -> Option<u8> {
    let addr: Ipv4Addr = ip.parse().ok()?;
    let octets = addr.octets();
    if octets[0] == SUBNET_PREFIX[0]
        && octets[1] == SUBNET_PREFIX[1]
        && octets[2] == SUBNET_PREFIX[2]
    {
        Some(octets[3])
    } else {
        None
    }
}

fn format_ip(host: u8) -> String {
    format!(
        "{}.{}.{}.{}",
        SUBNET_PREFIX[0], SUBNET_PREFIX[1], SUBNET_PREFIX[2], host
    )
}

#[cfg(test)]
#[path = "../../tests/src/network/ipam.rs"]
mod tests;
