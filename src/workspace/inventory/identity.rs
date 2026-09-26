//! How one host resource is named, and how a later check finds it again.
//!
//! The identity carries everything needed to re-check the resource, so a later
//! check never has to parse a description. A process is a pid *and* a start time,
//! because a pid on its own matches whatever process holds the number now.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::workspace::session;
use crate::workspace::storage;

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
            Self::Path { .. } => ResourceKind::Path,
        }
    }

    /// A stable string form, used only to order two inventories of the same state
    /// so they compare equal.
    pub(super) fn sort_key(&self) -> String {
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
                storage::loop_devices_for_image(backing_image)
                    .map(|devices| {
                        devices.iter().any(|present| present == device)
                            && storage::loop_device_is_mounted(device)
                    })
                    .unwrap_or(true)
            }
            Self::Veth { interface } => crate::network::teardown::veth_is_present(interface),
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
            Self::Path => "path",
        }
    }
}
