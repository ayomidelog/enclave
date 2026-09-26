//! The host capabilities the lifecycle depends on, each probed once.
//!
//! Every one of these is a property of the running kernel or of the programs installed
//! on the host, so none of them can change while the daemon runs. Probing one costs a
//! file read or a stat, and the probes that have to look further cache their answer, so
//! this is a report rather than a per-request cost.
//!
//! The report exists because the capabilities decide which tiers a host can run. A host
//! without overlay cannot share the cached rootfs; a host without the ext4 tools cannot
//! offer a disk quota; a host without cgroup v2 gets rlimit limits instead of cgroup
//! ones. Naming them together is what lets an operator see the whole answer at once
//! instead of discovering one missing piece per failed start.

use std::fs;

use crate::sandbox::cgroup;

use super::DoctorCheck;

/// The oldest kernel with idmapped mounts, which the workspace home bind uses.
const IDMAP_MINIMUM_KERNEL: (u32, u32) = (5, 12);

/// One capability and whether this host has it.
struct Capability {
    name: &'static str,
    available: bool,
    /// What is lost when it is missing.
    missing: &'static str,
    /// What the capability is, when the answer alone is not enough.
    ///
    /// A capability that is present can still be worth qualifying: cgroup v2 is either
    /// there or not, but which controllers it exposes decides whether a memory or CPU
    /// limit will work, and that is the part an operator has to check.
    detail: Option<String>,
}

impl Capability {
    fn report(&self) -> String {
        let answer = format!(
            "{}={}",
            self.name,
            if self.available { "yes" } else { "no" }
        );
        match self.detail.as_deref() {
            Some(detail) => format!("{answer} ({detail})"),
            None => answer,
        }
    }
}

/// Every capability, in the order the lifecycle needs them rather than alphabetically.
///
/// The cgroup row carries the available controllers, because a host with cgroup v2
/// but no memory controller is a different host from one with all three, and that is
/// the part an operator has to check before a memory limit will work.
fn capabilities() -> Vec<Capability> {
    vec![
        Capability {
            name: "cgroup_v2",
            available: cgroup::is_cgroup_v2_available(),
            missing: "resource limits will use rlimit only",
            detail: Some(controllers_detail()),
        },
        Capability {
            name: "overlay",
            available: kernel_supports_filesystem("overlay"),
            missing: "sandboxes will copy the cached rootfs instead of sharing it",
            detail: None,
        },
        Capability {
            name: "idmap",
            available: kernel_version().is_some_and(|version| version >= IDMAP_MINIMUM_KERNEL),
            missing: "workspace home mounts will not be idmapped",
            detail: None,
        },
        Capability {
            name: "disk_backend",
            available: crate::workspace::disk_backend_available().is_ok(),
            missing: "workspace disk quotas are unavailable",
            detail: None,
        },
        Capability {
            name: "iptables",
            available: crate::network::iptables_binary().is_ok(),
            missing: "workspace outbound networking is unavailable",
            detail: None,
        },
        Capability {
            name: "user_namespaces",
            available: crate::workspace::session::detect_user_namespace_mode().is_ok(),
            missing: "workspace runtimes launch without a user namespace",
            detail: None,
        },
    ]
}

/// Report every capability the lifecycle depends on, and what each missing one costs.
///
/// The status is a warning when anything is missing, because every one of these is
/// optional: a host without overlay still runs, it just copies. The detail names what
/// each missing capability costs rather than only that it is missing, because the
/// consequence is the part an operator has to decide about.
pub(crate) fn check_host_capabilities() -> DoctorCheck {
    const NAME: &str = "host_capabilities";
    let capabilities = capabilities();
    let report = capabilities
        .iter()
        .map(Capability::report)
        .collect::<Vec<_>>()
        .join(" ");
    let missing = capabilities
        .iter()
        .filter(|capability| !capability.available)
        .map(|capability| format!("{}: {}", capability.name, capability.missing))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return DoctorCheck::ok(NAME, &report);
    }
    DoctorCheck::warn(NAME, &format!("{report}; {}", missing.join("; ")))
}

/// The cgroup controllers this host exposes, when it has cgroup v2 at all.
///
/// A host with cgroup v2 but no memory controller is a different host from one with
/// all three: the second can enforce a memory limit and the first cannot. Naming the
/// controllers beside the answer is what makes the row actionable.
fn controllers_detail() -> String {
    let controllers = cgroup::available_controllers();
    if controllers.is_empty() {
        "no controllers exposed".to_string()
    } else {
        controllers.join(",")
    }
}

/// Whether the kernel advertises a filesystem type.
///
/// This is the kernel own list of the types it was built with, so it answers whether
/// the feature exists rather than whether one particular mount happened to work.
pub(crate) fn kernel_supports_filesystem(name: &str) -> bool {
    fs::read_to_string("/proc/filesystems")
        .map(|listed| {
            listed
                .lines()
                .any(|line| line.split_whitespace().last() == Some(name))
        })
        .unwrap_or(false)
}

/// The running kernel version, as its major and minor numbers.
///
/// The release string is the kernel own answer and carries a distribution suffix, so
/// only the leading two numbers are read and anything unparseable is reported as
/// unknown rather than guessed at.
fn kernel_version() -> Option<(u32, u32)> {
    let release = fs::read_to_string("/proc/sys/kernel/osrelease").ok()?;
    let mut numbers = release.trim().split('.');
    let major = numbers.next()?.parse().ok()?;
    let minor = numbers.next()?.parse().ok()?;
    Some((major, minor))
}
