use super::*;
use std::fs;

/// Move a path's modification time `seconds` into the past.
fn backdate(path: &std::path::Path, seconds: i64) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let raw = CString::new(path.as_os_str().as_bytes()).expect("path is not NUL-terminated");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock is after the epoch")
        .as_secs() as i64;
    let stamp = libc::timespec {
        tv_sec: now - seconds,
        tv_nsec: 0,
    };
    let result =
        unsafe { libc::utimensat(libc::AT_FDCWD, raw.as_ptr(), [stamp, stamp].as_ptr(), 0) };
    assert_eq!(result, 0, "failed to backdate {}", path.display());
}

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
fn registry_roundtrip_serialization() {
    let registry = Registry::default();
    let raw = serde_json::to_string_pretty(&registry).expect("serialize");
    let decoded: Registry = serde_json::from_str(&raw).expect("deserialize");
    assert_eq!(decoded.version, REGISTRY_VERSION);
    assert_eq!(decoded.generation, 0);
    assert!(decoded.sandboxes.is_empty());
}
#[test]
fn a_current_registry_needs_no_migration() {
    let mut registry = Registry::default();
    let steps = migrate(&mut registry).expect("a current registry migrates trivially");
    assert!(steps.is_empty());
    assert_eq!(registry.version, REGISTRY_VERSION);
}

#[test]
fn a_registry_written_before_the_version_field_was_enforced_is_migrated() {
    let mut registry = Registry {
        version: 0,
        ..Registry::default()
    };
    let steps = migrate(&mut registry).expect("version 0 is a supported older schema");
    assert_eq!(steps, vec![MigrationStep { from: 0, to: 1 }]);
    assert_eq!(registry.version, REGISTRY_VERSION);
}

#[test]
fn a_newer_schema_is_refused_by_the_migration_table() {
    let mut registry = Registry {
        version: REGISTRY_VERSION + 1,
        ..Registry::default()
    };
    let error = migrate(&mut registry).expect_err("a newer schema must not be migrated");
    assert!(error
        .to_string()
        .contains("newer than this binary supports"));
    assert_eq!(registry.version, REGISTRY_VERSION + 1);
}

#[test]
fn migrating_a_registry_keeps_every_record() {
    let mut registry = Registry {
        version: 0,
        ..Registry::default()
    };
    let sandbox = RegistrySandbox {
        metadata: SandboxMetadata::default(),
        workspaces: Default::default(),
    };
    registry.sandboxes.insert("sb".to_string(), sandbox);
    migrate(&mut registry).expect("migrate");
    assert!(registry.sandboxes.contains_key("sb"));
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
