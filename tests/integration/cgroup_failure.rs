//! A start whose cgroup setup fails must take the session it already launched down with it.
//!
//! A launch builds its resources in order: storage, then the runtime session, then the
//! cgroup that holds the session's processes, then auth, then networking. The session
//! therefore exists before the cgroup does, and a cgroup failure is the first failure in
//! the sequence that happens with a live runtime to release. Nothing else in the start
//! path rolls a runtime back, so this is the case where a missed rollback leaves a
//! process running that no record describes.
//!
//! The failure is injected through the kernel rather than through a hook. A cgroup has a
//! `cgroup.max.descendants` limit, which is the number of child cgroups it may have;
//! the kernel enforces it when a child is created, and it can be set from outside the
//! daemon. Setting it to zero on the sandbox cgroup makes the workspace cgroup impossible
//! to create while leaving the sandbox cgroup itself, and everything above it, working.

use std::fs;
use std::path::{Path, PathBuf};

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, update_sandbox_limits,
    BootstrapMethod, SandboxLimitsUpdate,
};
use enclave::workspace::{
    create_workspace, destroy_workspace, list_workspaces, start_workspace, WorkspaceLimits,
};

use super::support::{
    enclave_rules_by_interface, prepare_cached_rootfs, root_only, session_processes_for, state_dir,
};

fn sandbox_cgroup_path(sandbox_id: &str) -> PathBuf {
    Path::new("/sys/fs/cgroup").join(format!("enclave-sb-{sandbox_id}"))
}

/// Set the number of child cgroups a cgroup may have.
///
/// "max" is what the kernel calls "no limit"; it is what the value is set back to when
/// the test is done with the lever.
fn set_max_descendants(cgroup: &Path, value: &str) -> bool {
    fs::write(cgroup.join("cgroup.max.descendants"), value).is_ok()
}

/// A start whose cgroup setup fails must leave no runtime, no cgroup, and a stopped
/// workspace.
#[test]
#[ignore = "requires root privileges, cgroup v2, and namespace/mount support"]
fn a_start_whose_cgroup_setup_fails_stops_the_session_it_launched() {
    if !root_only() {
        return;
    }

    let Some(before) = enclave_rules_by_interface() else {
        return;
    };

    let state = state_dir("enclave-int-cgroup-failure");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-cgroup-failure-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    // The sandbox declares a memory limit so that it has a cgroup of its own. Without
    // one, the failed start would remove the sandbox cgroup as well, and the check that
    // the failed start left no workspace cgroup would have nothing to look at.
    update_sandbox_limits(
        &state,
        &sandbox.id,
        &SandboxLimitsUpdate {
            memory_bytes: Some(Some(256 * 1024 * 1024)),
            ..SandboxLimitsUpdate::default()
        },
    )
    .expect("give the sandbox a memory limit");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    let sandbox_path = PathBuf::from(&sandbox.sandbox_path);

    // The sandbox cgroup is the parent of the workspace cgroup, so it is the cgroup
    // whose descendant limit the workspace cgroup is subject to. A sandbox that has not
    // started a workspace yet has no cgroup, so it is created here before the limit is
    // set; the workspace launch would create the same one.
    let cgroup = sandbox_cgroup_path(&sandbox.id);
    fs::create_dir_all(&cgroup).expect("create the sandbox cgroup");
    assert!(
        set_max_descendants(&cgroup, "0"),
        "this test needs a kernel that enforces cgroup.max.descendants"
    );
    // The lever is only worth using if it is actually in force, so the value is read
    // back from the kernel rather than assumed from the write.
    assert_eq!(
        fs::read_to_string(cgroup.join("cgroup.max.descendants"))
            .expect("read the descendant limit")
            .trim(),
        "0"
    );

    let error = start_workspace(&state, &sandbox.id, &workspace.id)
        .expect_err("a start whose workspace cgroup cannot be created must fail");
    let message = format!("{error:#}");
    assert!(
        message.contains("cgroup"),
        "the failure must name the cgroup it could not create: {message}"
    );

    // The rollback is what this test is about. The session was launched before the
    // cgroup step, so a runtime left behind is the failure the rollback exists for.
    let orphans = session_processes_for(&sandbox_path);
    assert!(
        orphans.is_empty(),
        "the failed start left {} session process(es) behind: {orphans:?}",
        orphans.len()
    );

    // The workspace cgroup was never created, and the failed start did not leave one.
    // The sandbox cgroup is still there because the sandbox declares a limit, so this is
    // a statement about a cgroup that exists rather than about one that was removed, and
    // a removed cgroup would make the check pass for the wrong reason.
    assert!(
        cgroup.is_dir(),
        "the sandbox cgroup must survive the failed start"
    );
    assert!(
        fs::read_dir(&cgroup)
            .expect("read the sandbox cgroup")
            .flatten()
            .all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .starts_with("enclave-ws-")),
        "the failed start left a workspace cgroup behind"
    );

    // Networking runs after the cgroup step, so the failure happened before any
    // interface was made. A rule naming an interface is the evidence that one was.
    let after = enclave_rules_by_interface().expect("read rules after the failure");
    let added = after
        .iter()
        .filter(|(name, _)| !before.iter().any(|(known, _)| known == name))
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    assert!(
        added.is_empty(),
        "the failed start installed firewall rules for {added:?} before failing"
    );

    // The record describes a workspace that is stopped with nothing of the launch left
    // on it, so the operator's next command is a start rather than a repair.
    let record = list_workspaces(&state, Some(&sandbox.id))
        .expect("list workspaces")
        .into_iter()
        .find(|item| item.id == workspace.id)
        .expect("the test's workspace record is gone");
    assert_eq!(
        record.status.as_str(),
        "stopped",
        "the failed start left the workspace {}",
        record.status.as_str()
    );
    assert_eq!(
        record.runtime_pid, None,
        "the failed start left a runtime pid recorded"
    );
    assert_eq!(
        record.assigned_ip, None,
        "the failed start did not give its reserved address back"
    );

    // The probe is not vacuous. The same start succeeds once the descendant limit is
    // lifted, so the checks above describe a rollback rather than a workspace that
    // cannot start at all.
    assert!(
        set_max_descendants(&cgroup, "max"),
        "lift the descendant limit"
    );
    let started = start_workspace(&state, &sandbox.id, &workspace.id)
        .expect("the workspace must start once the descendant limit is lifted");
    assert!(started.runtime_pid.is_some());
    assert!(
        !session_processes_for(&sandbox_path).is_empty(),
        "a running workspace must have a session, so the orphan check above is not vacuous"
    );

    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
