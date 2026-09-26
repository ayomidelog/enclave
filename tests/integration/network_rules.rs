//! The firewall rules a workspace owns.

use std::fs;
use std::path::Path;

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, destroy_workspace, start_workspace, stop_workspace, WorkspaceLimits,
};

use super::support::{enclave_rules_by_interface, prepare_cached_rootfs, root_only, state_dir};

#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn starting_a_workspace_releases_its_dead_runtime_rules() {
    if !root_only() {
        return;
    }
    let Some(before) = enclave_rules_by_interface() else {
        return;
    };

    let state = state_dir("enclave-int-dead-runtime");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-dead-runtime-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let runtime_pid = started.runtime_pid.expect("runtime pid") as i32;

    // Kill the runtime the way a crash would, leaving the registry saying the
    // workspace is still running. The kernel releases the interface with the
    // namespace, but the rules that name it are Enclave's to remove.
    unsafe { libc::kill(runtime_pid, libc::SIGKILL) };
    for _ in 0..50 {
        if unsafe { libc::kill(runtime_pid, 0) } != 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    start_workspace(&state, &sandbox.id, &workspace.id)
        .expect("a start must release the dead runtime and succeed");

    let after = enclave_rules_by_interface().expect("read rules after the restart");
    // Every rule the probe's own run added is keyed by the workspace id hash, so
    // only interfaces new since the first snapshot belong to this test.
    let added = after
        .iter()
        .filter(|(interface, _)| !before.iter().any(|(known, _)| known == interface))
        .map(|(interface, _)| interface.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        added.len(),
        1,
        "the restarted workspace should own exactly one interface; found {added:?}"
    );
    assert!(
        Path::new("/sys/class/net").join(&added[0]).exists(),
        "the rule names interface {} which does not exist",
        added[0]
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
