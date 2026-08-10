use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use anyhow::{bail, Result};

pub const SUBNET_PREFIX: [u8; 3] = [10, 200, 0];
pub const GATEWAY_IP: &str = "10.200.0.1";
pub const SUBNET_CIDR: &str = "10.200.0.0/24";

const POOL_START: u8 = 10;

const POOL_END: u8 = 254;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpBitmap {
    bits: [u64; 4],
}

impl IpBitmap {
    pub fn from_used(used: &BTreeSet<u8>) -> Self {
        let mut bitmap = Self { bits: [0; 4] };
        for &octet in used {
            bitmap.mark(octet);
        }
        bitmap
    }

    pub fn mark(&mut self, octet: u8) {
        if !(POOL_START..=POOL_END).contains(&octet) {
            return;
        }
        let index = (octet - POOL_START) as usize;
        self.bits[index / 64] |= 1_u64 << (index % 64);
    }

    pub fn allocate(&self) -> Option<u8> {
        for (word_index, &word) in self.bits.iter().enumerate() {
            let available = !word;
            if available == 0 {
                continue;
            }
            let bit = available.trailing_zeros() as usize;
            let index = word_index * 64 + bit;
            let octet = POOL_START as usize + index;
            if octet <= POOL_END as usize {
                return Some(octet as u8);
            }
        }
        None
    }
}

pub fn allocate_ip(used: &BTreeSet<u8>) -> Result<String> {
    if let Some(octet) = IpBitmap::from_used(used).allocate() {
        return Ok(format_ip(octet));
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
