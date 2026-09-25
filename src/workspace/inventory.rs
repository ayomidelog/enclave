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

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::session;
use super::types::WorkspaceMetadata;

/// How one resource is identified on the host.
///
/// The identity carries everything needed to re-check the resource, so a later
/// check never has to parse a description. A process is a pid *and* a start time,
/// because a pid on its own matches whatever process holds the number now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResourceIdentity {
    Process {
        pid: u32,
        starttime_ticks: u64,
    },
    Cgroup {
        path: PathBuf,
    },
    Mount {
        mountpoint: PathBuf,
    },
    /// A loop device, named as the kernel names it, e.g. `/dev/loop3`.
    LoopDevice {
        device: String,
        backing_image: PathBuf,
    },
    /// The host end of a workspace veth pair.
    Veth {
        interface: String,
    },
    /// One Enclave-owned anti-spoofing rule, identified by what it matches.
    FirewallRule {
        chain: String,
        interface: String,
        address: String,
    },
    /// A file the workspace's runtime wrote.
    Path {
        path: PathBuf,
    },
}

impl ResourceIdentity {
    /// The kind of resource this identity names.
    pub fn kind(&self) -> ResourceKind {
        match self {
            Self::Process { .. } => ResourceKind::Process,
            Self::Cgroup { .. } => ResourceKind::Cgroup,
            Self::Mount { .. } => ResourceKind::Mount,
            Self::LoopDevice { .. } => ResourceKind::LoopDevice,
            Self::Veth { .. } => ResourceKind::Veth,
            Self::FirewallRule { .. } => ResourceKind::FirewallRule,
            Self::Path { .. } => ResourceKind::Path,
        }
    }

    /// A stable string form, used only to order two inventories of the same state
    /// so they compare equal.
    fn sort_key(&self) -> String {
        match self {
            Self::Process {
                pid,
                starttime_ticks,
            } => format!("process:{pid}:{starttime_ticks}"),
            Self::Cgroup { path } => format!("cgroup:{}", path.display()),
            Self::Mount { mountpoint } => format!("mount:{}", mountpoint.display()),
            Self::LoopDevice {
                device,
                backing_image,
            } => format!("loop_device:{device}:{}", backing_image.display()),
            Self::Veth { interface } => format!("veth:{interface}"),
            Self::FirewallRule {
                chain,
                interface,
                address,
            } => format!("firewall_rule:{chain}:{interface}:{address}"),
            Self::Path { path } => format!("path:{}", path.display()),
        }
    }

    /// Whether this resource is still on the host.
    pub fn still_present(&self) -> bool {
        match self {
            Self::Process {
                pid,
                starttime_ticks,
            } => session::process_matches(*pid, Some(*starttime_ticks)),
            Self::Cgroup { path } => path.exists(),
            Self::Mount { mountpoint } => crate::fsutil::is_mountpoint(mountpoint).unwrap_or(true),
            Self::LoopDevice {
                device,
                backing_image,
            } => {
                // An attached device is released when its last holder closes, so
                // the kernel can detach it after this check; a device with no
                // mount in any namespace is exactly the state a stop treats as
                // not-a-failure. Counting it here would contradict the
                // certificate's own `loop_device_absent` flag for the same
                // device, which asks this same question.
                super::storage::loop_devices_for_image(backing_image)
                    .map(|devices| {
                        devices.iter().any(|present| present == device)
                            && super::storage::loop_device_is_mounted(device)
                    })
                    .unwrap_or(true)
            }
            Self::Veth { interface } => crate::network::teardown::veth_is_present(interface),
            Self::FirewallRule {
                chain,
                interface,
                address,
            } => crate::network::nat::anti_spoof_chains_for(interface, address)
                .map(|chains| chains.iter().any(|present| *present == chain))
                .unwrap_or(true),
            Self::Path { path } => path.exists(),
        }
    }

    /// How the resource reads in a report.
    pub fn describe(&self) -> String {
        match self {
            Self::Process {
                pid,
                starttime_ticks,
            } => format!("runtime pid {pid} (start time {starttime_ticks})"),
            Self::Cgroup { path } => format!("cgroup {}", path.display()),
            Self::Mount { mountpoint } => format!("mount {}", mountpoint.display()),
            Self::LoopDevice {
                device,
                backing_image,
            } => format!("loop device {device} backing {}", backing_image.display()),
            Self::Veth { interface } => format!("interface {interface}"),
            Self::FirewallRule {
                chain,
                interface,
                address,
            } => format!("{chain} anti-spoof rule for {interface} ({address})"),
            Self::Path { path } => format!("file {}", path.display()),
        }
    }
}

/// The kind of host resource an inventory entry describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Process,
    Cgroup,
    Mount,
    LoopDevice,
    Veth,
    FirewallRule,
    Path,
}

impl ResourceKind {
    /// Lowercase label for a report.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::Cgroup => "cgroup",
            Self::Mount => "mount",
            Self::LoopDevice => "loop_device",
            Self::Veth => "veth",
            Self::FirewallRule => "firewall_rule",
            Self::Path => "path",
        }
    }
}

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
                if let Ok(chains) = crate::network::nat::anti_spoof_chains_for(&interface, ip) {
                    resources.extend(chains.into_iter().map(|chain| {
                        ResourceIdentity::FirewallRule {
                            chain: chain.to_string(),
                            interface: interface.clone(),
                            address: ip.to_string(),
                        }
                    }));
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
#[path = "../../tests/src/workspace/inventory.rs"]
mod tests;
