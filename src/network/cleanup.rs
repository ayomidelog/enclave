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
