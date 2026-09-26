pub mod bridge;
mod cleanup;
pub mod dns;
pub mod ipam;
pub mod nat;
pub mod publish;
pub mod teardown;
pub mod veth;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context, Result};

static HOST_NETWORKING_READY: AtomicBool = AtomicBool::new(false);
static HOST_NETWORKING_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Where the kernel exposes one directory per network interface.
///
/// Interface presence is answered from here rather than by running a tool: it is
/// the same information `ip link` prints, it costs one `stat`, and it cannot be
/// confused by the wording of a command's output. Both the teardown path and the
/// doctor read it, so the path is defined once.
pub(crate) const NET_CLASS_DIR: &str = "/sys/class/net";

/// The iptables binary this host will use, detected once.
///
/// The capability report asks this rather than probing on its own, so the answer is
/// the one the lifecycle will actually use and the probe is paid for once.
pub(crate) fn iptables_binary() -> Result<String> {
    nat::detect_iptables()
}

pub use cleanup::{
    teardown_workspace_network, teardown_workspace_networks, NetworkCleanupFailure,
    NetworkCleanupReport,
};

pub fn ensure_host_networking() -> Result<()> {
    let _guard = HOST_NETWORKING_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| anyhow::anyhow!("host networking setup lock poisoned"))?;
    if HOST_NETWORKING_READY.load(Ordering::SeqCst) && bridge::bridge_is_present()? {
        return Ok(());
    }
    bridge::ensure_bridge().context("failed to set up enclave bridge")?;
    nat::ensure_nat().context("failed to set up NAT")?;
    HOST_NETWORKING_READY.store(true, Ordering::SeqCst);
    Ok(())
}

/// Attach a workspace network at an address the caller already reserved.
///
/// A reservation is what makes concurrent starts safe: the address is chosen
/// while the registry lock is held, so two workspaces starting at the same time
/// cannot both take the first free one. The address is validated against the
/// Enclave subnet here because it comes from persisted state rather than from
/// this call.
pub fn setup_reserved_workspace_network(
    pid: u32,
    reserved_ip: &str,
    workspace_rootfs: &Path,
    workspace_id: &str,
) -> Result<String> {
    // The bridge and the NAT rules are shared by every workspace, so this is a
    // no-op after the first start. It is timed because it is on the critical path
    // of every start and a regression here would look like a slow veth setup.
    let host_ready = crate::perf::Timer::new("network.host_ready");
    ensure_host_networking()?;
    drop(host_ready);
    if ipam::parse_host_octet(reserved_ip).is_none() {
        bail!(
            "reserved workspace address {reserved_ip} is not inside the Enclave subnet {}",
            ipam::SUBNET_CIDR
        );
    }
    attach_workspace_network(pid, reserved_ip, workspace_rootfs, workspace_id)?;
    Ok(reserved_ip.to_string())
}

fn attach_workspace_network(
    pid: u32,
    ip: &str,
    workspace_rootfs: &Path,
    workspace_id: &str,
) -> Result<()> {
    let host_octet = ipam::parse_host_octet(ip).expect("allocate_ip returned invalid IP");
    let (veth_host, veth_peer) = veth::veth_names(host_octet, workspace_id);

    let result: Result<()> = (|| {
        let veth = crate::perf::Timer::new("network.veth");
        veth::setup_workspace_networking(pid, ip, &veth_host, &veth_peer)
            .with_context(|| format!("failed to set up networking for workspace (ip={ip})"))?;
        drop(veth);

        let rules = crate::perf::Timer::new("network.rules");
        nat::ensure_workspace_anti_spoofing(&veth_host, ip, workspace_id)
            .with_context(|| format!("failed to install anti-spoofing rules for {}", veth_host))?;
        drop(rules);

        let dns = crate::perf::Timer::new("network.dns");
        dns::provision_resolv_conf(workspace_rootfs)
            .with_context(|| "failed to provision DNS for workspace")?;
        dns::provision_etc_hosts(workspace_rootfs)
            .with_context(|| "failed to provision /etc/hosts for workspace")?;
        dns::provision_apt_sandbox_override(workspace_rootfs)
            .with_context(|| "failed to provision apt sandbox override for workspace")?;
        drop(dns);
        Ok(())
    })();
    if let Err(err) = result {
        let cleanup = teardown_workspace_network(ip, workspace_id);
        if cleanup.is_complete() {
            return Err(err);
        }
        return Err(err.context(format!(
            "workspace network rollback incomplete: {cleanup:?}"
        )));
    }

    Ok(())
}

pub fn collect_used_ips<'a, I>(ips: I) -> BTreeSet<u8>
where
    I: Iterator<Item = &'a str>,
{
    ips.filter_map(ipam::parse_host_octet).collect()
}

/// The host octets that another daemon's Enclave workspaces are already using.
///
/// The registry is not the only record of which addresses are taken. A second
/// daemon with its own state directory allocates from its own empty pool, and both
/// daemons attach to the same bridge, so the two can hand the same address to two
/// workspaces without either registry disagreeing with the other. The address
/// itself is inside a network namespace and invisible from the host, but the
/// interface name is not: a host veth is named for the octet it carries, so an
/// interface on the bridge names an octet that is in use, whoever created it.
///
/// The bridge membership is the part that makes this an answer rather than a guess.
/// A named interface that is not on the bridge is not necessarily another daemon's
/// workspace: it is also the shape of a leftover from a start that failed, which
/// this daemon's own next start replaces rather than routes around. Only a member of
/// the bridge is evidence of an address something else is holding.
///
/// Reading this is one directory listing, and it makes the allocator refuse an
/// address another daemon's workspace is holding rather than trusting a registry
/// that cannot see it.
pub fn host_veth_octets() -> BTreeSet<u8> {
    let Ok(entries) = std::fs::read_dir(NET_CLASS_DIR) else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| veth::is_enclave_veth_name(name))
        .filter(|name| veth::is_bridge_member(name))
        .filter_map(|name| veth::octet_from_veth_name(&name))
        .collect()
}

pub fn cleanup_host_networking() {
    let Ok(_guard) = HOST_NETWORKING_LOCK.get_or_init(|| Mutex::new(())).lock() else {
        tracing::warn!("host networking cleanup lock poisoned");
        return;
    };
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
        HOST_NETWORKING_READY.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[path = "../../tests/src/network/cleanup.rs"]
mod tests;
