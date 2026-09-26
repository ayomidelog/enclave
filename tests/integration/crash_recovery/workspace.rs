//! Crash points in a workspace lifecycle.
//!
//! A launch, a start that has launched but not committed, a stop, a network setup,
//! and a quota-backed stop. Each leaves a different kind of host state, and what
//! they share is that the record cannot describe all of it at the moment the
//! daemon dies.

use super::harness::{assert_recovered, reap, CrashFixture};
use crate::integration::support::{loop_devices_backing, root_only};
use std::time::Duration;

/// A daemon killed while a workspace is starting must leave a state the next start
/// recovers from.
///
/// The launch is the phase that has already created the runtime, its cgroup, the
/// interface, and the storage mounts by the time it is reached, so a crash there is the
/// case with the most to release. Recovery rolls the transition back rather than resuming
/// it, because the identity a half-finished launch recorded may not be the process that
/// is actually running.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_daemon_killed_during_a_workspace_launch_recovers() {
    if !root_only() {
        return;
    }

    let mut fixture = CrashFixture::new("launch");
    let child = fixture.spawn(&[
        "workspace",
        "start",
        &fixture.sandbox_name,
        &fixture.workspace_name,
    ]);
    assert!(
        fixture
            .daemon
            .wait_for_phase("workspace.start", "launch_runtime", Duration::from_secs(20)),
        "the start never reached its launch phase, so killing here would test nothing"
    );
    assert!(
        fixture.wait_for_session(Duration::from_secs(20)),
        "the launch never produced a session, so there is no runtime to leave behind"
    );
    fixture.daemon.kill();
    reap(child);
    fixture.daemon.start();

    assert_recovered(&fixture);
}

/// A daemon killed between a workspace's runtime launching and its metadata committing
/// must leave a state the next start recovers from.
///
/// This is the narrowest window in the lifecycle and the one with the most to get wrong:
/// the runtime exists and is running, and the registry still says the workspace is
/// starting. A recovery that trusted the record would leave a runtime nothing describes;
/// one that trusted the process would adopt a runtime whose identity was never committed.
/// What the daemon does instead is roll back, which is what this asserts.
///
/// The window is a few filesystem writes wide, which a fast host closes before a test
/// can react to the phase marker it watches for, so the test holds the registry lock
/// for the whole launch. Committing the runtime identity is the one step of a launch
/// that takes that lock, so the daemon cannot close the window while the guard is held,
/// and the status read below proves the kill landed on the near side of it.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_daemon_killed_before_a_workspace_start_commits_recovers() {
    if !root_only() {
        return;
    }

    let mut fixture = CrashFixture::new("commit");
    let child = fixture.spawn(&[
        "workspace",
        "start",
        &fixture.sandbox_name,
        &fixture.workspace_name,
    ]);
    // The reservation is taken under the registry lock before this phase is written,
    // so waiting for it is what keeps the guard from blocking the start before it has
    // a runtime to leave behind. The launch itself is still ahead, and the commit is
    // behind that.
    assert!(
        fixture
            .daemon
            .wait_for_phase("workspace.start", "launch_runtime", Duration::from_secs(20)),
        "the start never reached its launch phase, so killing here would test nothing"
    );
    let lock = fixture.hold_registry_lock();
    assert!(
        fixture.daemon.wait_for_phase(
            "workspace.start",
            "commit_runtime_metadata",
            Duration::from_secs(20)
        ),
        "the start never reached its commit phase, so killing here would test nothing"
    );
    assert_eq!(
        fixture.registry_workspace_status(),
        "starting",
        "the start committed its runtime metadata before the daemon was killed, so this \
         run tested nothing: the registry lock was not held across the commit"
    );
    fixture.daemon.kill();
    reap(child);
    drop(lock);
    fixture.daemon.start();

    assert_recovered(&fixture);
}

/// A daemon killed while a workspace is stopping must leave a state the next start
/// recovers from.
///
/// The stop phase is reached after the runtime has been signalled and before its cgroup,
/// its interface, its firewall rules, and its storage mounts have been released, so a
/// crash there leaves host state that the record still describes. Recovery reads that
/// record and finishes the teardown.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_daemon_killed_during_a_workspace_stop_recovers() {
    if !root_only() {
        return;
    }

    let mut fixture = CrashFixture::new("stop");
    fixture.cli_ok(&[
        "workspace",
        "start",
        &fixture.sandbox_name,
        &fixture.workspace_name,
    ]);
    let child = fixture.spawn(&[
        "workspace",
        "stop",
        &fixture.sandbox_name,
        &fixture.workspace_name,
    ]);
    assert!(
        fixture.daemon.wait_for_phase(
            "workspace.stop",
            "cleanup_resources",
            Duration::from_secs(20)
        ),
        "the stop never reached its cleanup phase, so killing here would test nothing"
    );
    fixture.daemon.kill();
    reap(child);
    fixture.daemon.start();

    assert_recovered(&fixture);
}

/// A daemon killed while a workspace's networking is being set up must leave a state
/// the next start recovers from.
///
/// The network phase runs inside the launch and has no journal phase of its own, so
/// the kill is timed from the interface appearing on the host. By then the veth
/// exists and the anti-spoofing rules are being installed, and the record still does
/// not name a runtime. A recovery that only rolled the record back would leave the
/// interface and its rules behind, because nothing else on the host knows they are
/// the workspace's.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_daemon_killed_during_network_setup_recovers() {
    if !root_only() {
        return;
    }

    let mut fixture = CrashFixture::new("network");
    let child = fixture.spawn(&[
        "workspace",
        "start",
        &fixture.sandbox_name,
        &fixture.workspace_name,
    ]);
    assert!(
        fixture.wait_for_veth(Duration::from_secs(20)),
        "the start never created an interface, so killing here would test nothing"
    );
    fixture.daemon.kill();
    reap(child);
    fixture.daemon.start();

    assert_recovered(&fixture);
}

/// A daemon killed while a quota-backed workspace is stopping must release the loop
/// device as well as the mount.
///
/// The quota tier is the one whose storage is a real filesystem: the stop unmounts
/// the image and then detaches the loop device behind it. A crash between the two
/// leaves the image attached with the record already rolled back, so nothing that
/// reads the registry can find it again.
#[test]
#[ignore = "requires root privileges, namespace/mount support, and loopback ext4 mounts"]
fn a_daemon_killed_during_a_quota_workspace_stop_recovers() {
    if !root_only() {
        return;
    }

    let mut fixture = CrashFixture::quota("quota-stop", 64);
    let image = fixture.workspace_dir().join("fs.img");
    fixture.cli_ok(&[
        "workspace",
        "start",
        &fixture.sandbox_name,
        &fixture.workspace_name,
    ]);
    assert!(
        !loop_devices_backing(&image).is_empty(),
        "the running quota workspace has to own a loop device for this to test anything"
    );

    let child = fixture.spawn(&[
        "workspace",
        "stop",
        &fixture.sandbox_name,
        &fixture.workspace_name,
    ]);
    assert!(
        fixture.daemon.wait_for_phase(
            "workspace.stop",
            "cleanup_resources",
            Duration::from_secs(20)
        ),
        "the stop never reached its cleanup phase, so killing here would test nothing"
    );
    fixture.daemon.kill();
    reap(child);
    fixture.daemon.start();

    assert_recovered(&fixture);
    assert!(
        loop_devices_backing(&image).is_empty(),
        "recovery left the workspace image attached to a loop device"
    );
}
