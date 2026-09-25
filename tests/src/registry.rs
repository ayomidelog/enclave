use super::*;
use std::fs;

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
