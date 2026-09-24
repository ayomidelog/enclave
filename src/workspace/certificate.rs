use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::session;
use super::types::WorkspaceMetadata;

/// One host resource that a workspace was expected to release but did not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupFailure {
    pub resource: String,
    pub detail: String,
}

/// Verified statement about the host resources a stopped workspace owned.
///
/// A registry mutation finishing is not evidence that the host is clean: the
/// runtime, its cgroup, its mounts, its loop device, its network, its published
/// ports, and its runtime markers are independent resources released by
/// independent operations. The certificate records what was actually verified,
/// so callers can tell "metadata deleted" apart from "host fully clean" and so a
/// stop that leaves something behind fails instead of reporting success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCleanupCertificate {
    pub workspace_id: String,
    pub runtime_exited: bool,
    pub cgroup_absent: bool,
    pub mounts_absent: bool,
    pub loop_device_absent: bool,
    pub runtime_files_removed: bool,
    /// Per-resource network outcome, when the caller had network state to tear
    /// down. The daemon owns the port publisher and fills the network and port
    /// fields after its own teardown step.
    #[serde(default)]
    pub network_complete: Option<bool>,
    #[serde(default)]
    pub ports_released: Option<bool>,
    #[serde(default)]
    pub failures: Vec<CleanupFailure>,
}

impl WorkspaceCleanupCertificate {
    /// Record the outcome of the caller's own port release step.
    pub fn with_ports_released(mut self, released: bool) -> Self {
        self.ports_released = Some(released);
        if !released {
            self.failures.push(CleanupFailure {
                resource: "ports".to_string(),
                detail: "published port listeners are still active".to_string(),
            });
        }
        self
    }

    /// Every mandatory check passed and no resource reported a failure.
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty()
            && self.runtime_exited
            && self.cgroup_absent
            && self.mounts_absent
            && self.loop_device_absent
            && self.runtime_files_removed
            && self.network_complete.unwrap_or(true)
            && self.ports_released.unwrap_or(true)
    }

    /// Render the failures as one line suitable for an error message.
    pub fn failure_summary(&self) -> String {
        self.failures
            .iter()
            .map(|failure| format!("{}: {}", failure.resource, failure.detail))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn record(&mut self, resource: &str, passed: bool, detail: impl Into<String>) {
        if !passed {
            self.failures.push(CleanupFailure {
                resource: resource.to_string(),
                detail: detail.into(),
            });
        }
    }
}

/// Verify the host resources Enclave owns for a stopped workspace.
///
/// `network` is the report from the network teardown step, which the caller runs
/// because it owns the network plan. Passing `None` records the network outcome
/// as unknown rather than clean.
pub(crate) fn verify_workspace_cleanup(
    workspace: &WorkspaceMetadata,
    network: Option<&crate::network::NetworkCleanupReport>,
) -> WorkspaceCleanupCertificate {
    let mut certificate = WorkspaceCleanupCertificate {
        workspace_id: workspace.id.clone(),
        runtime_exited: true,
        cgroup_absent: true,
        mounts_absent: true,
        loop_device_absent: true,
        runtime_files_removed: true,
        network_complete: network.map(|report| report.is_complete()),
        ports_released: None,
        failures: Vec::new(),
    };

    if let Some((pid, starttime)) = workspace.runtime_pid.zip(workspace.runtime_starttime_ticks) {
        certificate.runtime_exited = !session::process_matches(pid, Some(starttime));
        certificate.record(
            "runtime",
            certificate.runtime_exited,
            format!("pid {pid} is still alive after stop"),
        );
    }

    let cgroup = super::workspace_cgroup_path(&workspace.sandbox_id, &workspace.id);
    certificate.cgroup_absent = !cgroup.exists();
    certificate.record(
        "cgroup",
        certificate.cgroup_absent,
        format!("workspace cgroup {} still exists", cgroup.display()),
    );

    match crate::fsutil::MountInfoSnapshot::load() {
        Ok(snapshot) => {
            let remaining = snapshot.at_or_below(Path::new(&workspace.workspace_path));
            certificate.mounts_absent = remaining.is_empty();
            certificate.record(
                "mounts",
                certificate.mounts_absent,
                format!(
                    "{} mount(s) remain below {}: {}",
                    remaining.len(),
                    workspace.workspace_path,
                    remaining
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
        Err(error) => {
            certificate.mounts_absent = false;
            certificate.record(
                "mounts",
                false,
                format!("mount inventory unavailable: {error:#}"),
            );
        }
    }

    match super::storage::verify_disk_image_loop_detached(workspace) {
        Ok(()) => certificate.loop_device_absent = true,
        Err(error) => {
            certificate.loop_device_absent = false;
            certificate.record("loop_device", false, format!("{error:#}"));
        }
    }

    let pid_file = session::runtime_pid_file(workspace);
    let ready_file = session::runtime_ready_file(workspace);
    let leftover_markers = [pid_file, ready_file]
        .into_iter()
        .filter(|path| path.exists())
        .chain(
            session::namespace_ref_files_exist(workspace)
                .then(|| PathBuf::from(&workspace.namespace_refs.mount)),
        )
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    certificate.runtime_files_removed = leftover_markers.is_empty();
    certificate.record(
        "runtime_files",
        certificate.runtime_files_removed,
        format!("runtime markers remain: {}", leftover_markers.join(", ")),
    );

    if let Some(report) = network {
        if !report.is_complete() {
            certificate.record(
                "network",
                false,
                report
                    .failures
                    .iter()
                    .map(|failure| format!("{}: {}", failure.resource, failure.message))
                    .collect::<Vec<_>>()
                    .join("; "),
            );
        }
    }

    certificate
}

#[cfg(test)]
#[path = "../../tests/src/workspace/certificate.rs"]
mod tests;
