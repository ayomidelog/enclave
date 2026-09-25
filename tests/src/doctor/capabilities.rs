//! What the capability report has to say about the host it runs on.

use crate::doctor::capabilities::{check_host_capabilities, kernel_supports_filesystem};

/// The report names every capability, so a host that lacks one can be told which.
///
/// The names are part of what an operator reads, so they are pinned here: a rename
/// that reached only one side would leave the report describing a capability nobody
/// can look up.
#[test]
fn the_capability_report_names_every_capability_it_probes() {
    let check = check_host_capabilities();
    assert_eq!(check.name, "host_capabilities");
    for capability in [
        "cgroup_v2",
        "overlay",
        "idmap",
        "disk_backend",
        "iptables",
        "user_namespaces",
    ] {
        assert!(
            check.detail.contains(capability),
            "the report does not mention {capability}: {}",
            check.detail
        );
    }
}

/// A missing capability is a warning that says what it costs, not a bare "no".
///
/// Every capability is optional, so the report is a warning rather than an error. What
/// makes it actionable is the consequence beside each missing one, which is the part an
/// operator has to decide about.
#[test]
fn a_missing_capability_is_reported_with_its_consequence() {
    let check = check_host_capabilities();
    let missing = check.detail.contains("=no");
    assert_eq!(
        check.status,
        if missing { "warn" } else { "ok" },
        "the status must follow whether anything is missing: {}",
        check.detail
    );
    if missing {
        // The consequence follows the report, after the semicolon that separates them.
        let consequences = check
            .detail
            .split_once(';')
            .map(|(_, rest)| rest)
            .unwrap_or_default();
        assert!(
            !consequences.trim().is_empty(),
            "a missing capability must say what it costs: {}",
            check.detail
        );
    }
}

/// The filesystem probe answers from the kernel's own list.
///
/// It has to recognise something the kernel certainly built in and something it
/// certainly did not, or the report would be a constant.
#[test]
fn the_filesystem_probe_reads_the_kernel_list() {
    assert!(
        kernel_supports_filesystem("proc") || kernel_supports_filesystem("sysfs"),
        "the kernel must advertise at least one of its own filesystems"
    );
    assert!(
        !kernel_supports_filesystem("definitely-not-a-filesystem"),
        "the probe must not report an invented filesystem as available"
    );
}
