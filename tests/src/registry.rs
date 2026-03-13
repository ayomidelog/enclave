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
    assert!(decoded.sandboxes.is_empty());
}
