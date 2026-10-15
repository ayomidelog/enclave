//! What tearing one workspace network down proved, and the report it returns.
//!
//! A teardown releases several independent resources: the veth pair, the
//! anti-spoofing rules that name it, and the bridge and NAT the workspace shared
//! with the rest of the host. Each can fail on its own, so the report names which
//! ones are gone rather than reporting a single success.

use serde::{Deserialize, Serialize};

use super::*;

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
    let workers = crate::parallel::worker_count(
        "ENCLAVE_CLEANUP_WORKERS",
        workspaces.len(),
        crate::parallel::parallelism().clamp(1, 4),
    );
    // A report is always produced for every workspace: the worker always returns one,
    // and the `None` case below can only be a worker that did not run at all, which is
    // reported as a failure rather than dropped.
    crate::parallel::map_bounded(workspaces.to_vec(), workers, |(ip, id)| {
        teardown_workspace_network(&ip, &id)
    })
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
    .collect()
}
