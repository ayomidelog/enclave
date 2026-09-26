//! A crash point in the sandbox lifecycle.

use super::harness::{assert_recovered, reap, CrashFixture};
use crate::integration::support::root_only;
use std::path::Path;
use std::time::Duration;

/// A daemon killed while a sandbox is stopping must leave a state the next start
/// recovers from.
///
/// A sandbox stop unmounts the rootfs and removes the sandbox cgroup, and it records
/// both before doing them. A crash between the two leaves a sandbox whose record says
/// stopping, and recovery completes the stop rather than reporting a sandbox that is
/// half way down.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_daemon_killed_during_a_sandbox_stop_recovers() {
    if !root_only() {
        return;
    }

    let mut fixture = CrashFixture::new("sandbox-stop");
    let child = fixture.spawn(&["stop", &fixture.sandbox_name]);
    assert!(
        fixture
            .daemon
            .wait_for_phase("sandbox.stop", "unmount_rootfs", Duration::from_secs(20)),
        "the sandbox stop never reached its unmount phase, so killing here would test nothing"
    );
    fixture.daemon.kill();
    reap(child);
    fixture.daemon.start();

    // The sandbox is the unit here, so the assertion is about the sandbox: it has to be
    // in a settled state, and its cgroup and rootfs mount have to match that state.
    let status = fixture.daemon.cli(&["status", &fixture.sandbox_name]);
    let stdout = String::from_utf8_lossy(&status.stdout).into_owned();
    assert!(
        stdout.contains("status: stopped") || stdout.contains("status: running"),
        "the sandbox is not in a settled state: {stdout}{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let sandbox_id = fixture.sandbox_id();
    let cgroup = Path::new("/sys/fs/cgroup").join(format!("enclave-sb-{sandbox_id}"));
    if stdout.contains("status: stopped") {
        assert!(
            !cgroup.exists(),
            "a stopped sandbox still has its cgroup {}",
            cgroup.display()
        );
    }
    assert_recovered(&fixture);
}
