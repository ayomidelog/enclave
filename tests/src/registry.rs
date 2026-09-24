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
