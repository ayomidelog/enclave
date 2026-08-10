use super::*;
use std::fs;

#[test]
fn marker_paths_are_digest_and_index_scoped() {
    let rootfs = std::path::Path::new("/state/sandboxes/sandbox/rootfs");
    let digest = "a".repeat(64);
    let path = marker_path(rootfs, &digest, 3).unwrap();
    assert_eq!(
        path,
        std::path::PathBuf::from(format!(
            "/state/sandboxes/sandbox/runtime/setup-cache/{}-3.done",
            digest
        ))
    );
}

#[test]
fn completion_markers_are_atomic_and_detectable() {
    let root = std::env::temp_dir().join(format!(
        "enclave-setup-cache-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let rootfs = root.join("sandbox/rootfs");
    fs::create_dir_all(&rootfs).unwrap();
    let digest = "b".repeat(64);
    assert!(!is_complete(&rootfs, &digest, 0).unwrap());
    mark_complete(&rootfs, &digest, 0).unwrap();
    assert!(is_complete(&rootfs, &digest, 0).unwrap());
    assert!(!is_complete(&rootfs, &digest, 1).unwrap());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn invalid_digest_is_rejected() {
    let result = marker_path(std::path::Path::new("/state/rootfs"), "bad", 0);
    assert!(result.is_err());
}
