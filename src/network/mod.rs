pub mod bridge;
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
use serde::{Deserialize, Serialize};

static HOST_NETWORKING_READY: AtomicBool = AtomicBool::new(false);
static HOST_NETWORKING_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Where the kernel exposes one directory per network interface.
///
/// Interface presence is answered from here rather than by running a tool: it is
/// the same information `ip link` prints, it costs one `stat`, and it cannot be
/// confused by the wording of a command's output. Both the teardown path and the
/// doctor read it, so the path is defined once.
pub(crate) const NET_CLASS_DIR: &str = "/sys/class/net";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkCleanupFailure {
    pub resource: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkCleanupReport {
    pub workspace_id: String,
    pub assigned_ip: String,
    pub veth_host: Option<String>,
    pub anti_spoof_rules_absent: bool,
    pub veth_absent: bool,
    pub failures: Vec<NetworkCleanupFailure>,
}

impl NetworkCleanupReport {
    pub fn is_complete(&self) -> bool {
        self.veth_host.is_some()
            && self.anti_spoof_rules_absent
            && self.veth_absent
            && self.failures.is_empty()
    }

    fn failed(
        workspace_id: String,
        assigned_ip: String,
        resource: &str,
        error: impl std::fmt::Display,
    ) -> Self {
        Self {
            workspace_id,
            assigned_ip,
            veth_host: None,
            anti_spoof_rules_absent: false,
            veth_absent: false,
            failures: vec![NetworkCleanupFailure {
                resource: resource.to_string(),
                message: error.to_string(),
            }],
        }
    }
}

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

pub fn teardown_workspace_network(assigned_ip: &str, workspace_id: &str) -> NetworkCleanupReport {
    let mut report = NetworkCleanupReport {
        workspace_id: workspace_id.to_string(),
        assigned_ip: assigned_ip.to_string(),
        veth_host: None,
        anti_spoof_rules_absent: false,
        veth_absent: false,
        failures: Vec::new(),
    };
    let Some(host_octet) = ipam::parse_host_octet(assigned_ip) else {
        report.failures.push(NetworkCleanupFailure {
            resource: "workspace-ip".to_string(),
            message: format!("invalid Enclave workspace IP '{assigned_ip}'"),
        });
        return report;
    };
    let (veth_host, _) = veth::veth_names(host_octet, workspace_id);
    report.veth_host = Some(veth_host.clone());

    match nat::remove_workspace_anti_spoofing(&veth_host, assigned_ip) {
        Ok(()) => report.anti_spoof_rules_absent = true,
        Err(error) => report.failures.push(NetworkCleanupFailure {
            resource: "anti-spoofing-rules".to_string(),
            message: format!("{error:#}"),
        }),
    }
    match teardown::remove_veth(&veth_host) {
        Ok(()) => report.veth_absent = true,
        Err(error) => report.failures.push(NetworkCleanupFailure {
            resource: "veth".to_string(),
            message: format!("{error:#}"),
        }),
    }
    report
}

/// Tear down several workspace networks concurrently. The bridge and NAT are
/// shared resources, but veth and anti-spoofing cleanup is workspace-local.
pub fn teardown_workspace_networks(workspaces: &[(String, String)]) -> Vec<NetworkCleanupReport> {
    if workspaces.is_empty() {
        return Vec::new();
    }
    let worker_count = std::env::var("ENCLAVE_CLEANUP_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=64).contains(value))
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|parallelism| parallelism.get().clamp(1, 4))
                .unwrap_or(4)
        })
        .min(workspaces.len());
    let queue = std::sync::Arc::new(std::sync::Mutex::new(
        std::collections::VecDeque::from_iter(workspaces.iter().cloned().enumerate()),
    ));
    let reports = std::sync::Arc::new(std::sync::Mutex::new(
        (0..workspaces.len())
            .map(|_| None::<NetworkCleanupReport>)
            .collect::<Vec<_>>(),
    ));
    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = std::sync::Arc::clone(&queue);
            let reports = std::sync::Arc::clone(&reports);
            scope.spawn(move || loop {
                let Some((index, (ip, id))) =
                    queue.lock().ok().and_then(|mut queue| queue.pop_front())
                else {
                    return;
                };
                let report = teardown_workspace_network(&ip, &id);
                if let Ok(mut reports) = reports.lock() {
                    reports[index] = Some(report);
                }
            });
        }
    });
    let Ok(reports) = std::sync::Arc::try_unwrap(reports) else {
        return workspaces
            .iter()
            .map(|(ip, id)| {
                NetworkCleanupReport::failed(
                    id.clone(),
                    ip.clone(),
                    "cleanup-worker",
                    "network cleanup worker ownership remained active",
                )
            })
            .collect();
    };
    match reports.into_inner() {
        Ok(reports) => reports
            .into_iter()
            .enumerate()
            .map(|(index, report)| {
                report.unwrap_or_else(|| {
                    NetworkCleanupReport::failed(
                        workspaces[index].1.clone(),
                        workspaces[index].0.clone(),
                        "cleanup-worker",
                        "network cleanup worker did not return a result",
                    )
                })
            })
            .collect(),
        Err(_) => workspaces
            .iter()
            .map(|(ip, id)| {
                NetworkCleanupReport::failed(
                    id.clone(),
                    ip.clone(),
                    "cleanup-worker",
                    "network cleanup result lock was poisoned",
                )
            })
            .collect(),
    }
}

pub fn collect_used_ips<'a, I>(ips: I) -> BTreeSet<u8>
where
    I: Iterator<Item = &'a str>,
{
    ips.filter_map(ipam::parse_host_octet).collect()
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
