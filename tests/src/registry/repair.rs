//! What a repair adopts, what it removes, and what it leaves alone.
//!
//! The two cases that matter are the ones a repair must not touch: a directory a live
//! process is creating, and a workspace directory a live runtime still owns.

use super::*;

/// A staging directory is where a create builds a directory before renaming it
/// into place, so it is not part of the registry and no scan reads it. One left
/// by a create that died is garbage, but a fresh one may still belong to a live
/// create, so the sweep has to be age-guarded rather than unconditional.
#[test]
fn repair_sweeps_an_abandoned_creation_staging_directory() {
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-registry-staging-sweep-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = fs::remove_dir_all(&state_dir);
    let staging_root = crate::fsutil::creation_staging_root(&state_dir).join("sandbox");
    let fresh = staging_root.join("sandbox-fresh");
    let abandoned = staging_root.join("sandbox-abandoned");
    fs::create_dir_all(&fresh).expect("create a fresh staging directory");
    fs::create_dir_all(&abandoned).expect("create an abandoned staging directory");
    fs::write(
        abandoned.join(crate::fsutil::CREATION_MARKER_NAME),
        format!("pid={}\nstarttime=1\n", u32::MAX),
    )
    .expect("write a stale marker");
    backdate(&abandoned, 300);

    repair_registry(&state_dir, false).expect("repair should succeed");

    assert!(
        fresh.exists(),
        "a staging directory inside the grace period may belong to a live create"
    );
    assert!(
        !abandoned.exists(),
        "a staging directory left by a dead create is swept"
    );
    let _ = fs::remove_dir_all(&state_dir);
}

#[test]
fn strict_repair_ignores_rootfs_cache_directory() {
    let state_dir =
        std::env::temp_dir().join(format!("enclave-registry-repair-{}", std::process::id()));
    let _ = fs::remove_dir_all(&state_dir);
    fs::create_dir_all(state_dir.join("sandboxes/rootfs-cache/base")).expect("create rootfs cache");

    let report = repair_registry(&state_dir, true).expect("strict repair should skip rootfs-cache");
    assert_eq!(report.added_sandboxes, 0);
    assert_eq!(report.removed_sandboxes, 0);
    assert_eq!(report.added_workspaces, 0);
    assert_eq!(report.removed_workspaces, 0);

    let _ = fs::remove_dir_all(state_dir);
}

#[test]
fn repair_leaves_a_sandbox_that_a_live_process_is_creating() {
    // The registry record for a sandbox only appears once its rootfs is in place,
    // so during a bootstrap the directory exists with no `sandbox.json`. Repair
    // must not read that as a leftover while the create is still running, or two
    // concurrent creates would delete each other's work.
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-registry-creating-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = fs::remove_dir_all(&state_dir);
    let sandbox_dir = state_dir.join("sandboxes").join("in-progress-abc123");
    fs::create_dir_all(sandbox_dir.join("rootfs")).expect("create the in-progress sandbox");
    crate::fsutil::write_creation_marker(&sandbox_dir).expect("mark the directory");

    let report = repair_registry(&state_dir, false).expect("repair should succeed");
    assert_eq!(report.removed_sandboxes, 0);
    assert_eq!(
        report.added_sandboxes, 0,
        "an uncommitted create is not a record"
    );
    assert!(
        sandbox_dir.exists(),
        "repair deleted a sandbox directory a live process is still creating"
    );

    // With the marker gone and the process still alive, the same directory is a
    // leftover again and repair removes it as before.
    crate::fsutil::remove_creation_marker(&sandbox_dir);
    let report = repair_registry(&state_dir, false).expect("repair should succeed");
    assert_eq!(report.removed_sandboxes, 0);
    assert!(
        !sandbox_dir.exists(),
        "a directory with no marker and no metadata is a leftover"
    );
    let _ = fs::remove_dir_all(&state_dir);
}

#[test]
fn repair_retains_a_workspace_directory_a_live_runtime_still_owns() {
    // `workspace.json` is the only file that records a runtime's pid. Losing it
    // used to make repair delete the directory, leaving the runtime, its cgroup,
    // its interface, and its firewall rules owned by nothing. The directory's own
    // namespace marker is what proves a live owner, so repair must retain it and
    // say why.
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-registry-live-orphan-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = fs::remove_dir_all(&state_dir);
    let sandbox_dir = state_dir.join("sandboxes").join("sb-live");
    let workspace_dir = sandbox_dir.join("workspaces").join("ws-live");
    fs::create_dir_all(workspace_dir.join("ns")).expect("create workspace dir");
    // The sandbox itself has to look real, or repair removes the whole sandbox
    // directory before it ever reaches the workspace.
    let metadata = crate::sandbox::SandboxMetadata {
        id: "sb-live".to_string(),
        name: "sb-live".to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: crate::sandbox::BootstrapMethod::CachedRootfs,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        sandbox_path: sandbox_dir.to_string_lossy().to_string(),
        rootfs_path: sandbox_dir.join("rootfs").to_string_lossy().to_string(),
        rootfs_lower_path: None,
        mounted_rootfs_path: sandbox_dir
            .join("runtime/rootfs.mnt")
            .to_string_lossy()
            .to_string(),
        workspaces_path: sandbox_dir.join("workspaces").to_string_lossy().to_string(),
        home_base_path: sandbox_dir.join("home-base").to_string_lossy().to_string(),
        limits: crate::sandbox::SandboxLimits::default(),
        status: crate::sandbox::SandboxStatus::Stopped,
    };
    fs::create_dir_all(sandbox_dir.join("rootfs")).expect("create rootfs dir");
    fs::write(
        sandbox_dir.join("sandbox.json"),
        serde_json::to_string_pretty(&metadata).expect("serialize sandbox metadata"),
    )
    .expect("write sandbox metadata");
    let namespace = fs::read_link("/proc/self/ns/pid").expect("read own pid namespace");
    fs::write(
        workspace_dir.join("ns/pid.ref"),
        format!("{}\n", namespace.to_string_lossy()),
    )
    .expect("write the namespace marker");

    let report = repair_registry(&state_dir, false).expect("repair should succeed");
    assert!(
        workspace_dir.exists(),
        "repair deleted a directory a live runtime still owns"
    );
    assert_eq!(
        report.retained_orphans.len(),
        1,
        "{:?}",
        report.retained_orphans
    );
    let retained = &report.retained_orphans[0];
    assert_eq!(retained.workspace_id, "ws-live");
    let discovered = retained
        .runtime
        .runtime_pid
        .expect("a pid in the namespace");
    assert_eq!(
        fs::read_link(format!("/proc/{discovered}/ns/pid"))
            .expect("read the discovered process namespace")
            .to_string_lossy(),
        namespace.to_string_lossy()
    );
    assert!(
        retained.describe().contains("ws-live"),
        "{}",
        retained.describe()
    );

    // Once nothing owns the directory it is a leftover again, and repair removes
    // it as it always did.
    fs::write(workspace_dir.join("ns/pid.ref"), "unassigned\n").expect("clear the marker");
    let report = repair_registry(&state_dir, false).expect("repair should succeed");
    assert!(
        report.retained_orphans.is_empty(),
        "{:?}",
        report.retained_orphans
    );
    assert!(
        !workspace_dir.exists(),
        "a directory with no live owner is a leftover"
    );
    let _ = fs::remove_dir_all(&state_dir);
}

/// A registry this binary does not understand must be refused, not rebuilt.
///
/// Repair rebuilds a registry it cannot load, which is the right answer for a
/// corrupt file: the sandboxes tree is the authority and the record is a cache of
/// it. It is the wrong answer for a file written by a newer Enclave, because that
/// file parses and says so, and rebuilding it writes this binary's schema over
/// whatever that version recorded. The two failures reach repair as the same kind
/// of error, so the file itself is what has to tell them apart.
#[test]
fn repair_refuses_a_registry_from_a_newer_version() {
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-registry-future-schema-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = fs::remove_dir_all(&state_dir);
    fs::create_dir_all(&state_dir).expect("create state dir");

    // A registry that parses and declares a version from the future, with a
    // sandbox the rebuild would otherwise drop.
    let registry = Registry {
        version: REGISTRY_VERSION + 1,
        ..Registry::default()
    };
    fs::write(
        registry_path(&state_dir),
        serde_json::to_string_pretty(&registry).expect("serialize"),
    )
    .expect("write the future registry");

    let error = repair_registry(&state_dir, false)
        .expect_err("repair must refuse a schema it does not understand");
    assert!(
        format!("{error:#}").contains("newer than this binary supports"),
        "the refusal must name the version: {error:#}"
    );

    // The file is left exactly as it was found: a repair that refuses is not a
    // repair that partially wrote.
    let after: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(registry_path(&state_dir)).expect("read the registry back"),
    )
    .expect("the registry is still valid json");
    assert_eq!(
        after["version"].as_u64(),
        Some(u64::from(REGISTRY_VERSION + 1)),
        "repair rewrote a schema it refused"
    );

    // A corrupt file, which is the case repair is for, is still rebuilt.
    fs::write(registry_path(&state_dir), "{ not json").expect("corrupt the registry");
    repair_registry(&state_dir, false).expect("repair rebuilds a corrupt registry");
    let rebuilt: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(registry_path(&state_dir)).expect("read the rebuilt registry"),
    )
    .expect("the rebuilt registry is valid json");
    assert_eq!(
        rebuilt["version"].as_u64(),
        Some(u64::from(REGISTRY_VERSION))
    );

    let _ = fs::remove_dir_all(&state_dir);
}
