//! A sandbox's disk budget, and the workspace allocations it covers.
//!
//! The budget is what makes a sandbox disk size a number an operator can set: a
//! sandbox has no image of its own, so what it actually allocates is the sum of its
//! workspaces' quota images. These are the rules that keep that sum inside the budget
//! without making a resize that replaces an existing allocation look like a new one.

use super::*;

/// A sandbox whose disk budget is `mib` MiB.
///
/// The tests are written in MiB because that is the unit an operator sets and the unit
/// the messages name; the stored value is bytes.
fn budget(mib: u64) -> SandboxLimits {
    SandboxLimits {
        disk_bytes: Some(mib * 1024 * 1024),
        ..SandboxLimits::default()
    }
}

/// The same conversion for an allocation passed to the checks.
fn mib(mib: u64) -> u64 {
    mib * 1024 * 1024
}

/// A sandbox with no budget accepts anything, which is what an operator who never
/// set one expects.
#[test]
fn no_budget_accepts_any_allocation() {
    SandboxLimits::default()
        .check_disk_budget(0, u64::MAX / 2)
        .expect("an unlimited sandbox must accept any allocation");
}

#[test]
fn an_allocation_within_the_budget_is_accepted() {
    budget(4096)
        .check_disk_budget(mib(1024), mib(2048))
        .expect("1024 + 2048 fits in 4096");
}

#[test]
fn an_allocation_that_exceeds_the_budget_is_refused_by_name() {
    let error = budget(4096)
        .check_disk_budget(mib(2048), mib(4096))
        .expect_err("2048 + 4096 does not fit in 4096");
    let message = format!("{error:#}");
    // The message has to name the numbers an operator has to act on: the budget, what
    // is already held, what was asked for, and how far over it goes.
    assert!(message.contains("4096 MiB"), "{message}");
    assert!(message.contains("2048 MiB"), "{message}");
    assert!(message.contains("exceed"), "{message}");
    assert!(message.contains("resize"), "{message}");
}

/// A resize that replaces an allocation is measured on the difference.
///
/// The workspace being resized is excluded from what the others hold, so growing one
/// workspace in a sandbox that is exactly full is allowed when the new size still fits.
#[test]
fn a_resize_is_measured_on_what_the_other_workspaces_hold() {
    // Two workspaces of 1024 in a 4096 budget: the other holds 1024, so this one may
    // grow to 3072 but not to 4096.
    budget(4096)
        .check_disk_budget(mib(1024), mib(3072))
        .expect("1024 + 3072 is exactly the budget");
    assert!(budget(4096)
        .check_disk_budget(mib(1024), mib(3073))
        .is_err());
}

#[test]
fn an_exactly_full_budget_is_allowed() {
    budget(2048)
        .check_disk_budget(mib(1024), mib(1024))
        .expect("a budget is a limit, so exactly filling it is inside it");
}

/// A budget below what the sandbox already allocates is refused rather than stored.
///
/// Accepting it would leave the sandbox over its own limit the moment it was set, and
/// nothing would bring it back inside until a workspace was shrunk or destroyed.
#[test]
fn a_budget_below_the_current_allocation_is_refused() {
    let error = budget(1024)
        .check_disk_budget_covers(mib(2048))
        .expect_err("a budget under the current total must be refused");
    let message = format!("{error:#}");
    assert!(message.contains("2048 MiB"), "{message}");
    assert!(message.contains("1024 MiB"), "{message}");
    assert!(message.contains("shrink"), "{message}");
}

#[test]
fn a_budget_that_covers_the_current_allocation_is_accepted() {
    budget(2048)
        .check_disk_budget_covers(mib(2048))
        .expect("a budget equal to the allocation covers it");
    budget(4096)
        .check_disk_budget_covers(mib(2048))
        .expect("a larger budget covers it");
}

/// Clearing the budget removes the limit rather than setting it to zero.
#[test]
fn clearing_the_budget_accepts_any_allocation_again() {
    let mut limits = budget(1024);
    assert!(limits.check_disk_budget(0, mib(4096)).is_err());
    limits
        .apply_update(&SandboxLimitsUpdate {
            disk_bytes: Some(None),
            ..SandboxLimitsUpdate::default()
        })
        .expect("clearing the budget is a valid update");
    assert_eq!(limits.disk_bytes, None);
    limits
        .check_disk_budget(0, 4096)
        .expect("an unlimited sandbox must accept any allocation");
}

/// A limit the runtime cannot start inside is refused where it is set.
#[test]
fn a_memory_limit_below_the_floor_is_refused() {
    let mut limits = SandboxLimits::default();
    let error = limits
        .apply_update(&SandboxLimitsUpdate {
            memory_bytes: Some(Some(1024)),
            ..SandboxLimitsUpdate::default()
        })
        .expect_err("a 1 KiB memory limit must be refused");
    assert!(format!("{error:#}").contains("at least"), "{error:#}");
}
