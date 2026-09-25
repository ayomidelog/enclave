//! A failure in the middle of building a workspace network must leave nothing behind.
//!
//! A start builds a workspace's network in steps: the veth pair, the anti-spoofing rules
//! that name it, and the resolver files inside the workspace. Only the first two are host
//! state, and they are the two that outlive a failed start if the rollback does not run:
//! the interface keeps its name and the rules keep dropping traffic for an address nothing
//! uses. The rollback is what makes a failed start indistinguishable from one that never
//! began, and this is the only failure point in that sequence that a test can reach from
//! outside the process.

use std::fs;

use enclave::sandbox::{create_sandbox, start_sandbox, stop_sandbox, BootstrapMethod};
use enclave::workspace::{create_workspace, destroy_workspace, start_workspace, WorkspaceLimits};

use super::support::{
    enclave_rules_by_interface, prepare_cached_rootfs, root_only, session_processes_for, state_dir,
    SandboxCleanup,
};

fn workspace_record(state: &std::path::Path, workspace_id: &str) -> (String, Option<String>) {
    enclave::registry::with_registry(state, |registry| {
        let workspace = registry
            .sandboxes
            .values()
            .find_map(|sandbox| sandbox.workspaces.get(workspace_id))
            .expect("the test's workspace record is gone");
        Ok((
            workspace.status.as_str().to_string(),
            workspace.assigned_ip.clone(),
        ))
    })
    .expect("read the registry")
}

/// A start that fails after the veth and its rules are installed must roll both back.
///
/// The failure is injected where it is real rather than through a hook: the resolver file
/// in the sandbox rootfs is replaced by a directory, so the provisioning step cannot write
/// it. That step runs after the interface and the rules, which is the case the rollback
/// exists for.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_start_that_fails_after_the_veth_and_rules_rolls_both_back() {
    if !root_only() {
        return;
    }

    let Some(before) = enclave_rules_by_interface() else {
        return;
    };

    let state = state_dir("enclave-int-dns-rollback");
    prepare_cached_rootfs(&state, "bookworm");
    let mut cleanup = SandboxCleanup::new(state.clone());
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-dns-rollback-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    cleanup.record(&sandbox.id);
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");

    // The workspace root is the sandbox rootfs the session pivots into, so a directory
    // placed at the resolver path there is what the provisioning step will try to write.
    let rootfs = std::path::PathBuf::from(&sandbox.mounted_rootfs_path);
    let resolv = rootfs.join("etc/resolv.conf");
    fs::create_dir_all(resolv.parent().expect("the resolver path has a parent"))
        .expect("create the rootfs etc directory");
    fs::create_dir(&resolv).expect("occupy the resolver path with a directory");

    let error = start_workspace(&state, &sandbox.id, &workspace.id)
        .expect_err("a start whose resolver cannot be written must fail");
    let message = format!("{error:#}");
    assert!(
        message.contains("resolv.conf"),
        "the failure must name the file it could not write: {message}"
    );

    // The rollback is what this test is about. A rule names the interface it applies to, so
    // a rule that still exists after the failure is an interface Enclave created and did
    // not release.
    let after = enclave_rules_by_interface().expect("read rules after the failure");
    let added = after
        .iter()
        .filter(|(name, _)| !before.iter().any(|(known, _)| known == name))
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    assert!(
        added.is_empty(),
        "the failed start left firewall rules for {added:?}"
    );

    // The session the start launched is gone as well. The rollback stops it, and a
    // session that outlived the failure would hold the namespaces and the mounts the
    // rest of the rollback is releasing.
    let orphans = session_processes_for(&std::path::PathBuf::from(&sandbox.sandbox_path));
    assert!(
        orphans.is_empty(),
        "the failed start left {} session process(es) behind: {orphans:?}",
        orphans.len()
    );

    // The workspace is stopped with its address given back, so a later start is the
    // operator's next command rather than a repair.
    let (status, assigned_ip) = workspace_record(&state, &workspace.id);
    assert_eq!(
        status, "stopped",
        "the failed start left the workspace {status} holding {assigned_ip:?}"
    );
    assert_eq!(
        assigned_ip, None,
        "the failed start did not give its reserved address back"
    );

    // And it starts once the resolver path is usable again.
    fs::remove_dir(&resolv).expect("remove the blocking directory");
    let started = start_workspace(&state, &sandbox.id, &workspace.id)
        .expect("the workspace must start once the resolver can be written");
    assert!(started.runtime_pid.is_some());

    // The probe is not vacuous: a start that succeeds does install rules naming its own
    // interface, so an empty result after the failure means the rules were released
    // rather than never installed.
    let after_success = enclave_rules_by_interface().expect("read rules after the success");
    let owned = after_success
        .iter()
        .filter(|(name, _)| !before.iter().any(|(known, _)| known == name))
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        owned.len(),
        1,
        "a running workspace must own exactly one interface; found {owned:?}"
    );
    assert!(
        !session_processes_for(&std::path::PathBuf::from(&sandbox.sandbox_path)).is_empty(),
        "a running workspace must have a session, so the orphan check above is not vacuous"
    );

    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    drop(cleanup);
}
