//! Shared fixtures and host probes for the privileged integration tests.
//!
//! Every test builds its own state directory and its own cached rootfs, so these
//! are about constructing a sandbox the daemon can start rather than about
//! sharing state between tests. The host probes are here for the same reason:
//! several test files ask the host the same question about a cgroup, a mount, or
//! a process, and answering it in one place keeps the answers consistent.
//!
//! The parts are separate modules because a test reaches for them at different
//! times: fixtures build the state, host asks the kernel what is left of it,
//! daemon owns the process under test, and layout finds what the daemon named.

mod daemon;
mod fixtures;
mod host;
mod layout;

pub(super) use daemon::TestDaemon;
pub(super) use fixtures::{prepare_cached_rootfs, state_dir, SandboxCleanup};
pub(super) use host::{
    cgroup_processes, enclave_rules_by_interface, ext4_filesystem_size, is_mountpoint,
    loop_devices_backing, mounts_at_or_below, persistent_helper_is_running, process_starttime,
    session_processes_for, workspace_cgroup_path,
};
pub(super) use layout::{sandbox_dir, workspace_dir};

/// Whether the test process is running as root.
///
/// The privileged suites are marked `#[ignore]` and the daemon refuses to run
/// without root, so a test that is run without privileges returns early rather
/// than failing on a permission error that says nothing about the code.
pub(super) fn root_only() -> bool {
    unsafe { libc::geteuid() == 0 }
}
