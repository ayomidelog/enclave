use super::*;
use std::fs;

fn sandbox_fixture(root: &std::path::Path) -> SandboxMetadata {
    SandboxMetadata {
        id: "sandbox-id".to_string(),
        name: "sandbox".to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: super::super::BootstrapMethod::CachedRootfs,
        created_at: "2026-09-01T00:00:00Z".to_string(),
        sandbox_path: root.to_string_lossy().to_string(),
        rootfs_path: root.join("rootfs").to_string_lossy().to_string(),
        rootfs_lower_path: None,
        mounted_rootfs_path: root
            .join("runtime/rootfs.mnt")
            .to_string_lossy()
            .to_string(),
        workspaces_path: root.join("workspaces").to_string_lossy().to_string(),
        home_base_path: root.join("home-base").to_string_lossy().to_string(),
        limits: Default::default(),
        status: Default::default(),
    }
}

fn commands(list: &[&str]) -> Vec<String> {
    list.iter().map(|command| (*command).to_string()).collect()
}

/// Every input that changes what a setup command does has to change the key, or
/// a stale result is reused for a different definition.
#[test]
fn the_setup_key_covers_every_input_that_changes_the_result() {
    let root = std::env::temp_dir().join(format!(
        "enclave-setup-key-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(root.join("rootfs")).unwrap();
    let sandbox = sandbox_fixture(&root);
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    let base = commands(&["apt-get update", "apt-get install -y curl"]);
    let key = key_for(&state, &sandbox, &base);
    assert_eq!(key.len(), 64, "the key is a sha256 hex digest");

    // The same inputs produce the same key, so a repeat run is a cache hit.
    assert_eq!(key, key_for(&state, &sandbox, &base));

    // A changed mirror changes the rootfs the commands run against.
    let mut other_mirror = sandbox_fixture(&root);
    other_mirror.mirror = "https://mirror.example/debian".to_string();
    assert_ne!(key, key_for(&state, &other_mirror, &base));

    // A changed suite or bootstrap method changes the base content.
    let mut other_suite = sandbox_fixture(&root);
    other_suite.suite = "trixie".to_string();
    assert_ne!(key, key_for(&state, &other_suite, &base));

    // A changed command changes the work to do.
    assert_ne!(
        key,
        key_for(
            &state,
            &sandbox,
            &commands(&["apt-get update", "apt-get install -y git"])
        )
    );

    // Reordering the same commands changes the work to do as well.
    assert_ne!(
        key,
        key_for(
            &state,
            &sandbox,
            &commands(&["apt-get install -y curl", "apt-get update"])
        )
    );

    // Removing a command changes the work to do.
    assert_ne!(
        key,
        key_for(&state, &sandbox, &commands(&["apt-get update"]))
    );

    let _ = fs::remove_dir_all(root);
}

/// Re-importing the cached rootfs changes what the commands run against, so the
/// recorded results must stop counting as hits.
#[test]
fn the_setup_key_changes_when_the_base_rootfs_is_replaced() {
    let root = std::env::temp_dir().join(format!(
        "enclave-setup-base-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(root.join("rootfs")).unwrap();
    // A real rootfs has content; the identity has to notice the content changing
    // even when the directory is recreated at the same path.
    fs::write(root.join("rootfs/marker"), b"before").unwrap();
    let sandbox = sandbox_fixture(&root);
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    let base = commands(&["apt-get update"]);
    let before = key_for(&state, &sandbox, &base);

    // Replace the tree, as a re-import would.
    fs::remove_dir_all(root.join("rootfs")).unwrap();
    fs::create_dir_all(root.join("rootfs")).unwrap();
    fs::write(root.join("rootfs/marker"), b"after").unwrap();

    assert_ne!(
        before,
        key_for(&state, &sandbox, &base),
        "a replaced rootfs must not reuse the previous results"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn marker_paths_are_digest_and_index_scoped() {
    let sandbox = std::path::Path::new("/state/sandboxes/sandbox");
    let digest = "a".repeat(64);
    let path = marker_path(sandbox, &digest, 3).unwrap();
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
    let sandbox = root.join("sandbox");
    fs::create_dir_all(sandbox.join("rootfs")).unwrap();
    let digest = "b".repeat(64);
    assert!(!is_complete(&sandbox, &digest, 0).unwrap());
    mark_complete(&sandbox, &digest, 0).unwrap();
    assert!(is_complete(&sandbox, &digest, 0).unwrap());
    assert!(!is_complete(&sandbox, &digest, 1).unwrap());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn invalid_digest_is_rejected() {
    let result = marker_path(std::path::Path::new("/state/rootfs"), "bad", 0);
    assert!(result.is_err());
}
