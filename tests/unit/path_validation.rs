use std::fs;
use std::path::PathBuf;

use enclave::fsutil::{canonicalize_within, ensure_path_within};

fn temp_dir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{}-{}", prefix, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn canonicalization_resolves_valid_child_path() {
    let base = temp_dir("enclave-unit-canon");
    fs::create_dir_all(base.join("snapshots")).expect("create snapshots");

    let path = canonicalize_within(
        &base,
        &PathBuf::from("snapshots/../snapshots"),
        "snapshot path",
    )
    .expect("canonicalization should stay within base");

    assert!(path.starts_with(&base));
    let _ = fs::remove_dir_all(base);
}

#[test]
fn detects_symlink_escape_outside_base() {
    let base = temp_dir("enclave-unit-symlink");
    let outside = temp_dir("enclave-unit-outside");
    let link = base.join("evil");
    std::os::unix::fs::symlink(&outside, &link).expect("create symlink");

    let result = ensure_path_within(&base, &PathBuf::from("evil/escape.txt"), "sandbox path");
    assert!(result.is_err());

    let _ = fs::remove_dir_all(base);
    let _ = fs::remove_dir_all(outside);
}

#[test]
fn rejects_snapshot_traversal_path() {
    let base = temp_dir("enclave-unit-snapshot-path");
    let result = ensure_path_within(
        &base,
        &PathBuf::from("snapshots/../../etc/passwd"),
        "snapshot path",
    );
    assert!(result.is_err());
    let _ = fs::remove_dir_all(base);
}
