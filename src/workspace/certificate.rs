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
/// The default certificate verifies nothing, so it is incomplete and fails
/// closed. It exists as the deserialization fallback for a response that predates
/// the field, where "no evidence" must not read as "verified clean".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Whether the workspace directory itself is gone.
    ///
    /// A stop keeps the directory, so this is left unset there; a destroy has to
    /// prove it is gone.
    #[serde(default)]
    pub files_removed: Option<bool>,
    #[serde(default)]
    pub failures: Vec<CleanupFailure>,
    /// What the workspace owned before its teardown ran.
    ///
    /// The per-resource flags above say which checks passed. This says what the
    /// checks were run against, so an operator can see the inventory a cleanup
    /// started from and the report can prove the diff is empty rather than only
    /// that each named check succeeded.
    #[serde(default)]
    pub inventory: Option<super::inventory::ResourceInventory>,
}

impl WorkspaceCleanupCertificate {
    /// Record the inventory the teardown started from, and fail for every resource
    /// that is still on the host.
    ///
    /// This is the check that turns "the cleanup calls returned success" into
    /// "the resources are gone". A resource that survives is named by kind and
    /// identity, so the report is actionable rather than a count.
    pub fn with_inventory(mut self, inventory: super::inventory::ResourceInventory) -> Self {
        for resource in inventory.surviving() {
            self.failures.push(CleanupFailure {
                resource: resource.kind().as_str().to_string(),
                detail: format!("{} was not released", resource.describe()),
            });
        }
        self.inventory = Some(inventory);
        self
    }

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
            && self.files_removed.unwrap_or(true)
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

/// Verify the host resources Enclave owns for a destroyed workspace.
///
/// The passed record is the workspace as it was immediately before deletion,
/// which is the only remaining description of what it owned. A destroy adds two
/// things a stop does not need to prove: the workspace directory is gone, and the
/// network teardown succeeded. Everything else is the same verified statement the
/// stop certificate makes, which is what lets a destroy claim the host is clean
/// rather than only that the files were removed.
pub(crate) fn verify_workspace_destroyed(
    workspace: &WorkspaceMetadata,
    network_complete: bool,
) -> WorkspaceCleanupCertificate {
    let mut certificate = verify_workspace_cleanup(workspace, None);
    certificate.network_complete = Some(network_complete);

    let workspace_dir = Path::new(&workspace.workspace_path);
    let files_removed = !workspace_dir.exists();
    certificate.files_removed = Some(files_removed);
    certificate.record(
        "files",
        files_removed,
        format!(
            "workspace directory {} still exists",
            workspace_dir.display()
        ),
    );

    // Prove the network state is gone by looking, rather than trusting the
    // teardown report the caller passed in. A veth can outlive a delete that
    // reported success when a namespace still holds it, and a firewall rule can
    // survive a delete that matched nothing.
    if let Some(ip) = workspace.assigned_ip.as_deref() {
        if let Some(octet) = crate::network::ipam::parse_host_octet(ip) {
            let (veth_host, _) = crate::network::veth::veth_names(octet, &workspace.id);
            let veth_absent = !crate::network::teardown::veth_is_present(&veth_host);
            certificate.record(
                "veth",
                veth_absent,
                format!("interface {veth_host} is still present"),
            );
            match crate::network::nat::anti_spoof_chains_for(&veth_host, ip) {
                Ok(chains) if chains.is_empty() => {}
                Ok(chains) => certificate.record(
                    "firewall",
                    false,
                    format!(
                        "anti-spoofing rule for {veth_host} still present in {}",
                        chains.join(", ")
                    ),
                ),
                Err(error) => certificate.record(
                    "firewall",
                    false,
                    format!("could not verify rules for {veth_host}: {error:#}"),
                ),
            }
        }
    }

    certificate
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
        files_removed: None,
        failures: Vec::new(),
        inventory: None,
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
            let root = Path::new(&workspace.workspace_path);
            // The certificate answers for the mounts Enclave created. A mount
            // Enclave did not create is not a resource this workspace owned, so
            // it is reported rather than counted as an Enclave cleanup failure.
            let owned = snapshot.owned_at_or_below(root);
            let foreign = snapshot.foreign_at_or_below(root);
            certificate.mounts_absent = owned.is_empty();
            certificate.record(
                "mounts",
                certificate.mounts_absent,
                format!(
                    "{} Enclave mount(s) remain below {}",
                    owned.len(),
                    workspace.workspace_path
                ),
            );
            if !foreign.is_empty() {
                tracing::warn!(
                    "workspace '{}': {} mount(s) below {} were not created by Enclave and were left in place: {}",
                    workspace.id,
                    foreign.len(),
                    workspace.workspace_path,
                    foreign.join("; ")
                );
            }
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
