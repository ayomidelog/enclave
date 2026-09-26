//! Re-checking the host after a teardown, to turn a successful call into
//! evidence that the resource is gone.

use std::path::{Path, PathBuf};

use super::record::WorkspaceCleanupCertificate;
use crate::workspace::session;
use crate::workspace::types::WorkspaceMetadata;

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
            let firewall_timer = crate::perf::Timer::new("certificate.firewall");
            let chains = crate::network::nat::anti_spoof_chains_for(&veth_host, ip);
            drop(firewall_timer);
            match chains {
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

    let cgroup = crate::workspace::workspace_cgroup_path(&workspace.sandbox_id, &workspace.id);
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

    match crate::workspace::storage::verify_disk_image_loop_detached(workspace) {
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
