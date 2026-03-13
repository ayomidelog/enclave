pub mod bridge;
pub mod dns;
pub mod ipam;
pub mod nat;
pub mod publish;
pub mod teardown;
pub mod veth;

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};

pub fn ensure_host_networking() -> Result<()> {
    bridge::ensure_bridge().context("failed to set up enclave bridge")?;
    nat::ensure_nat().context("failed to set up NAT")?;
    Ok(())
}

pub fn setup_workspace_network(pid: u32, used_ips: &BTreeSet<u8>, rootfs: &Path) -> Result<String> {
    ensure_host_networking()?;

    let ip = ipam::allocate_ip(used_ips)?;
    let host_octet = ipam::parse_host_octet(&ip).expect("allocate_ip returned invalid IP");
    let (veth_host, veth_peer) = veth::veth_names(host_octet);

    let result: Result<()> = (|| {
        veth::setup_workspace_networking(pid, &ip, &veth_host, &veth_peer)
            .with_context(|| format!("failed to set up networking for workspace (ip={ip})"))?;
        nat::ensure_workspace_anti_spoofing(&veth_host, &ip)
            .with_context(|| format!("failed to install anti-spoofing rules for {}", veth_host))?;

        dns::provision_resolv_conf(rootfs)
            .with_context(|| "failed to provision DNS for workspace")?;
        dns::provision_etc_hosts(rootfs)
            .with_context(|| "failed to provision /etc/hosts for workspace")?;
        dns::provision_apt_sandbox_override(rootfs)
            .with_context(|| "failed to provision apt sandbox override for workspace")?;
        Ok(())
    })();
    if let Err(err) = result {
        if let Err(cleanup_err) = nat::remove_workspace_anti_spoofing(&veth_host, &ip) {
            tracing::warn!(
                "failed to remove anti-spoofing rules for {} during rollback: {cleanup_err:#}",
                veth_host
            );
        }
        teardown::remove_veth(&veth_host);
        return Err(err);
    }

    Ok(ip)
}

pub fn teardown_workspace_network(assigned_ip: &str) {
    if let Some(host_octet) = ipam::parse_host_octet(assigned_ip) {
        let (veth_host, _) = veth::veth_names(host_octet);
        if let Err(err) = nat::remove_workspace_anti_spoofing(&veth_host, assigned_ip) {
            tracing::warn!(
                "failed to remove anti-spoofing rules for {}: {err:#}",
                veth_host
            );
        }
        teardown::remove_veth(&veth_host);
    }
}

pub fn collect_used_ips<'a, I>(ips: I) -> BTreeSet<u8>
where
    I: Iterator<Item = &'a str>,
{
    ips.filter_map(ipam::parse_host_octet).collect()
}

pub fn cleanup_host_networking() {
    let bridge_removed = match bridge::remove_bridge_if_idle() {
        Ok(removed) => removed,
        Err(err) => {
            tracing::warn!("failed to remove idle bridge: {err:#}");
            false
        }
    };

    if bridge_removed {
        if let Err(err) = nat::remove_nat() {
            tracing::warn!("failed to remove NAT rule: {err:#}");
        }
    }
}
