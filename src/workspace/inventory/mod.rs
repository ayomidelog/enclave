//! The host resources one workspace owns, and the proof that it released them.
//!
//! A cleanup function returning `Ok` means the calls it made did not report a
//! failure. It is not evidence that the host is clean, because the resources a
//! workspace owns are released by independent operations: the runtime by a
//! signal, its cgroup by a directory removal, its storage by an unmount, its loop
//! device by the kernel after the last reference closes, its interface by the
//! namespace dying, and its firewall rules by an `iptables` call. Any one of them
//! can succeed while another silently does not.
//!
//! An inventory is the list of what a workspace owned, captured from the host
//! rather than from the code path that created it. Recording it before teardown
//! and re-checking it afterwards turns "the calls returned success" into "the
//! resources are gone", which is the difference between a cleanup that looks
//! right and one that is right.
//!
//! The firewall rules are deliberately not in the inventory. Answering "does this
//! workspace's anti-spoofing rule still exist" reads the whole filter table, and
//! the network teardown already answers it twice: once to decide what to delete
//! and once to verify the deletion, with that verification feeding the cleanup
//! certificate. Probing the same table twice more, once before the teardown and
//! once after, cost more than every other resource in the inventory together and
//! could not fail a cleanup that the teardown's own verification had not already
//! failed.
//!
//! How one resource is named lives in the identity module; collecting the list
//! and diffing it afterwards is here.

mod identity;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::session;
use super::types::WorkspaceMetadata;

pub use identity::{ResourceIdentity, ResourceKind};

/// What one workspace owned when the inventory was taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceInventory {
    pub workspace_id: String,
    /// The host resources, ordered so two inventories of the same state compare
    /// equal.
    pub resources: Vec<ResourceIdentity>,
}

impl ResourceInventory {
    /// Collect the resources `workspace` owns right now.
    ///
    /// Every read is best effort: a resource that cannot be inspected is not in
    /// the inventory, because the inventory's job is to name what can be proven
    /// and a guess would make the later diff meaningless. The runtime identity
    /// comes from the caller's record, because a process that is already gone has
    /// nothing left to release.
    pub fn collect(sandbox_id: &str, workspace: &WorkspaceMetadata) -> Self {
        let mut resources = Vec::new();

        if let Some((pid, starttime_ticks)) =
            workspace.runtime_pid.zip(workspace.runtime_starttime_ticks)
        {
            if session::process_matches(pid, Some(starttime_ticks)) {
                resources.push(ResourceIdentity::Process {
                    pid,
                    starttime_ticks,
                });
            }
        }

        let cgroup = super::runtime_limits::workspace_cgroup_path(sandbox_id, &workspace.id);
        if cgroup.exists() {
            resources.push(ResourceIdentity::Cgroup { path: cgroup });
        }

        if let Ok(snapshot) = crate::fsutil::MountInfoSnapshot::load() {
            for mountpoint in snapshot.owned_at_or_below(Path::new(&workspace.workspace_path)) {
                resources.push(ResourceIdentity::Mount {
                    mountpoint: PathBuf::from(mountpoint),
                });
            }
        }

        if super::storage::workspace_uses_disk_image(workspace) {
            let image = super::storage::workspace_disk_image_path(workspace);
            if let Ok(devices) = super::storage::loop_devices_for_image(&image) {
                resources.extend(
                    devices
                        .into_iter()
                        .map(|device| ResourceIdentity::LoopDevice {
                            device,
                            backing_image: image.clone(),
                        }),
                );
            }
        }

        if let Some(ip) = workspace.assigned_ip.as_deref() {
            if let Some(octet) = crate::network::ipam::parse_host_octet(ip) {
                let (interface, _) = crate::network::veth::veth_names(octet, &workspace.id);
                if crate::network::teardown::veth_is_present(&interface) {
                    resources.push(ResourceIdentity::Veth {
                        interface: interface.clone(),
                    });
                }
            }
        }

        for path in runtime_marker_paths(workspace) {
            if path.exists() {
                resources.push(ResourceIdentity::Path { path });
            }
        }

        resources.sort_by_key(ResourceIdentity::sort_key);
        resources.dedup();
        Self {
            workspace_id: workspace.id.clone(),
            resources,
        }
    }

    /// Whether nothing was owned when the inventory was taken.
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }

    /// How many resources of `kind` the inventory holds.
    pub fn count(&self, kind: ResourceKind) -> usize {
        self.resources
            .iter()
            .filter(|resource| resource.kind() == kind)
            .count()
    }

    /// The recorded resources that still exist on the host.
    ///
    /// This is the diff that makes a destroy trustworthy: an empty result means
    /// every resource the workspace owned is gone, and a non-empty one names
    /// exactly what survived rather than reporting that "something" failed.
    pub fn surviving(&self) -> Vec<&ResourceIdentity> {
        self.resources
            .iter()
            .filter(|resource| resource.still_present())
            .collect()
    }

    /// One line naming every surviving resource, or a statement that none did.
    pub fn surviving_summary(&self) -> String {
        let surviving = self.surviving();
        if surviving.is_empty() {
            return format!("all {} recorded resource(s) released", self.resources.len());
        }
        format!(
            "{} of {} recorded resource(s) survived: {}",
            surviving.len(),
            self.resources.len(),
            surviving
                .iter()
                .map(|resource| resource.describe())
                .collect::<Vec<_>>()
                .join("; ")
        )
    }
}

/// The files the workspace's runtime wrote that describe its own state.
fn runtime_marker_paths(workspace: &WorkspaceMetadata) -> Vec<PathBuf> {
    let mut paths = vec![
        session::runtime_pid_file(workspace),
        session::runtime_ready_file(workspace),
    ];
    let (mount_ref, pid_ref) = session::namespace_ref_paths(workspace);
    paths.push(mount_ref);
    paths.push(pid_ref);
    paths
}

#[cfg(test)]
#[path = "../../../tests/src/workspace/inventory.rs"]
mod tests;
