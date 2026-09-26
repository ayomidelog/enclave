use std::fmt;
use std::str::FromStr;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::resource_limits::{validate_cpu_percent, validate_memory_bytes};

pub const DEFAULT_DEBIAN_SUITE: &str = "bookworm";
pub const DEFAULT_DEBIAN_MIRROR: &str = "http://deb.debian.org/debian";

/// A byte count in MiB, rounded down, for a message an operator reads.
pub(crate) fn mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapMethod {
    #[default]
    Debootstrap,
    CachedRootfs,
}

impl fmt::Display for BootstrapMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BootstrapMethod::Debootstrap => write!(f, "debootstrap"),
            BootstrapMethod::CachedRootfs => write!(f, "cached_rootfs"),
        }
    }
}

impl FromStr for BootstrapMethod {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "debootstrap" => Ok(BootstrapMethod::Debootstrap),
            "cached_rootfs" => Ok(BootstrapMethod::CachedRootfs),
            _ => bail!(
                "unknown bootstrap method '{}'; valid values: debootstrap, cached_rootfs",
                s
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SandboxStatus {
    /// A start or resume transition is in progress.
    Starting,
    Running,
    Paused,
    /// A stop transition is in progress.
    Stopping,
    #[default]
    Stopped,
}

impl SandboxStatus {
    /// Lowercase label, for command output and lifecycle reports.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
        }
    }

    /// A lifecycle operation is in flight.
    pub fn is_transitional(&self) -> bool {
        matches!(self, Self::Starting | Self::Stopping)
    }

    /// The sandbox rootfs is expected to be mounted right now.
    pub fn rootfs_is_mounted(&self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Running | Self::Paused | Self::Stopping
        )
    }

    /// Workspaces are expected to be usable.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Running | Self::Paused)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SandboxLimits {
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<u64>,
    pub max_processes: Option<u64>,
    /// The most workspace disk this sandbox's workspaces may allocate between them.
    ///
    /// This is a budget rather than a size: a sandbox rootfs is a shared lower layer
    /// on the host filesystem, and what Enclave actually allocates per sandbox is the
    /// sum of its workspaces' quota images. Capping that sum is what makes a sandbox
    /// disk size something an operator can set, raise, and lower.
    ///
    /// It is enforced where an allocation is granted — creating a workspace with
    /// `disk_mb`, and resizing one up — so a sandbox can never hold more than its
    /// budget, and lowering the budget below what is already allocated is refused
    /// rather than silently exceeded.
    #[serde(default)]
    pub disk_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct SandboxLimitsUpdate {
    pub cpu_percent: Option<Option<f64>>,
    pub memory_bytes: Option<Option<u64>>,
    pub max_processes: Option<Option<u64>>,
    pub disk_bytes: Option<Option<u64>>,
}

impl SandboxLimits {
    pub fn validate(&self) -> Result<()> {
        if let Some(cpu_percent) = self.cpu_percent {
            validate_cpu_percent(cpu_percent)?;
        }
        validate_memory_bytes(self.memory_bytes)?;
        Ok(())
    }

    pub fn has_limits(&self) -> bool {
        self.cpu_percent.is_some() || self.memory_bytes.is_some() || self.max_processes.is_some()
    }

    /// Refuse an allocation that would take the sandbox past its disk budget.
    ///
    /// `already_allocated` is what the sandbox's workspaces hold apart from the one
    /// asking, so a resize that replaces an existing allocation is measured on the
    /// difference rather than on the sum of both. A sandbox with no budget accepts
    /// anything, which is what an operator who never set one expects.
    pub fn check_disk_budget(&self, already_allocated: u64, requested_bytes: u64) -> Result<()> {
        let Some(budget) = self.disk_bytes else {
            return Ok(());
        };
        let total = already_allocated.saturating_add(requested_bytes);
        if total > budget {
            bail!(
                "the sandbox disk budget is {} MiB and its other workspaces already hold {} MiB, so a {} MiB allocation would exceed it by {} MiB; raise the budget with `enclave resize <sandbox> --disk-mb N` or lower the allocation",
                mib(budget),
                mib(already_allocated),
                mib(requested_bytes),
                mib(total - budget),
            );
        }
        Ok(())
    }

    /// Refuse a budget that is smaller than what the sandbox already allocates.
    ///
    /// A budget below the current total is not a limit, it is a contradiction: the
    /// workspaces holding the space already exist, and nothing would bring the sandbox
    /// back inside the number until one of them is shrunk or destroyed. Saying so is
    /// more useful than accepting a value the sandbox already violates.
    pub fn check_disk_budget_covers(&self, allocated_bytes: u64) -> Result<()> {
        let Some(budget) = self.disk_bytes else {
            return Ok(());
        };
        if allocated_bytes > budget {
            bail!(
                "the sandbox's workspaces already allocate {} MiB, which is more than the requested {} MiB budget; shrink or destroy a workspace first, or set a budget of at least {} MiB",
                mib(allocated_bytes),
                mib(budget),
                mib(allocated_bytes),
            );
        }
        Ok(())
    }

    pub fn apply_update(&mut self, update: &SandboxLimitsUpdate) -> Result<bool> {
        let mut changed = false;
        if let Some(cpu_percent) = update.cpu_percent {
            if let Some(value) = cpu_percent {
                validate_cpu_percent(value)?;
            }
            changed |= self.cpu_percent != cpu_percent;
            self.cpu_percent = cpu_percent;
        }
        if let Some(memory_bytes) = update.memory_bytes {
            changed |= self.memory_bytes != memory_bytes;
            self.memory_bytes = memory_bytes;
        }
        if let Some(max_processes) = update.max_processes {
            changed |= self.max_processes != max_processes;
            self.max_processes = max_processes;
        }
        if let Some(disk_bytes) = update.disk_bytes {
            changed |= self.disk_bytes != disk_bytes;
            self.disk_bytes = disk_bytes;
        }
        self.validate()?;
        Ok(changed)
    }
}

impl SandboxLimitsUpdate {
    pub fn is_empty(&self) -> bool {
        self.cpu_percent.is_none()
            && self.memory_bytes.is_none()
            && self.max_processes.is_none()
            && self.disk_bytes.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SandboxMetadata {
    pub id: String,
    pub name: String,
    pub suite: String,
    pub mirror: String,
    #[serde(default)]
    pub bootstrap_method: BootstrapMethod,
    pub created_at: String,
    pub sandbox_path: String,
    pub rootfs_path: String,
    /// Immutable shared base layer for this sandbox rootfs.
    ///
    /// When set, `rootfs_path` is an OverlayFS mount whose lower layer is this
    /// directory, so creating a sandbox from a cached rootfs does not copy the
    /// whole tree. Writes land in the sandbox's own upper layer and never reach
    /// the shared base.
    #[serde(default)]
    pub rootfs_lower_path: Option<String>,
    #[serde(default)]
    pub mounted_rootfs_path: String,
    #[serde(default)]
    pub workspaces_path: String,
    #[serde(default)]
    pub home_base_path: String,
    #[serde(default)]
    pub limits: SandboxLimits,
    #[serde(default)]
    pub status: SandboxStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxListItem {
    pub id: String,
    pub name: String,
    pub status: SandboxStatus,
    pub workspace_count: usize,
}

/// What `sandbox.resize` changed.
///
/// The limits before and after are carried rather than only the new ones, so the
/// caller can report a change instead of restating a number. Every field is optional
/// because an omitted limit was left alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxResizeReport {
    pub sandbox: SandboxMetadata,
    #[serde(default)]
    pub previous_memory_bytes: Option<u64>,
    #[serde(default)]
    pub previous_disk_bytes: Option<u64>,
    #[serde(default)]
    pub previous_max_processes: Option<u64>,
}

/// Which base-image backend a sandbox root filesystem is on.
///
/// This is the one property of a sandbox that decides what creating it cost, and
/// it is a durable choice rather than a per-operation one: a sandbox either was
/// created from the cache as an overlay or had its rootfs copied, and it stays
/// that way for its whole life. It is reported as a value rather than as a
/// sentence so a caller can branch on it, with the sentence in the command
/// output where a person reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootfsTier {
    /// The rootfs is an OverlayFS mount whose lower layer is the shared cached
    /// rootfs, so creating the sandbox did not copy the tree and writes never
    /// reach the shared base.
    SharedOverlay,
    /// The rootfs is a private copy of the tree, which is the fallback for a host
    /// where the overlay cannot be set up.
    Copied,
}

impl RootfsTier {
    /// Lowercase label, for command output and reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SharedOverlay => "shared_overlay",
            Self::Copied => "copied",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxStatusReport {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub status: SandboxStatus,
    pub rootfs_path: String,
    /// Which backend the rootfs is on, and the base it shares when it shares one.
    pub rootfs_tier: RootfsTier,
    /// The shared immutable base layer, when this sandbox has one.
    #[serde(default)]
    pub rootfs_lower_path: Option<String>,
    pub rootfs_disk_usage_bytes: u64,
    pub workspace_count: usize,
    #[serde(default)]
    pub limits: SandboxLimits,
}
