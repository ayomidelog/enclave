use super::*;

#[test]
fn snapshot_name_validation_blocks_traversal() {
    assert!(validate_snapshot_name("snap_123").is_ok());
    assert!(validate_snapshot_name("..").is_err());
    assert!(validate_snapshot_name("snap..evil").is_err());
    assert!(validate_snapshot_name("snap/name").is_err());
    assert!(validate_snapshot_name("snap.name").is_err());
}

#[test]
fn snapshot_name_rejects_empty() {
    assert!(validate_snapshot_name("").is_err());
}

#[test]
fn snapshot_name_rejects_too_long() {
    let long_name: String = "a".repeat(64);
    assert!(validate_snapshot_name(&long_name).is_err());
}

#[test]
fn snapshot_name_accepts_max_length() {
    let name: String = "a".repeat(63);
    assert!(validate_snapshot_name(&name).is_ok());
}

#[test]
fn snapshot_name_accepts_dashes_and_underscores() {
    assert!(validate_snapshot_name("snap-2024-01-01").is_ok());
    assert!(validate_snapshot_name("snap_backup_v2").is_ok());
    assert!(validate_snapshot_name("my-snap-123").is_ok());
}

#[test]
fn snapshot_name_rejects_special_characters() {
    assert!(validate_snapshot_name("snap name").is_err());
    assert!(validate_snapshot_name("snap@host").is_err());
    assert!(validate_snapshot_name("snap!").is_err());
    assert!(validate_snapshot_name("snap#1").is_err());
}

#[test]
fn snapshot_name_rejects_single_dot() {
    assert!(validate_snapshot_name(".").is_err());
}

#[test]
fn default_snapshot_name_has_correct_prefix() {
    let name = default_snapshot_name();
    assert!(
        name.starts_with("snap-"),
        "default snapshot name should start with snap-: {name}"
    );
}

#[test]
fn default_snapshot_name_is_valid() {
    let name = default_snapshot_name();
    assert!(
        validate_snapshot_name(&name).is_ok(),
        "default name should be valid: {name}"
    );
}

#[test]
fn copy_dir_recursive_handles_empty_src() {
    let dir = std::env::temp_dir().join(format!("enclave-snap-test-{}", std::process::id()));
    let src = dir.join("empty-src");
    let dst = dir.join("dst");
    fs::create_dir_all(&src).unwrap();

    let result = copy_dir_recursive(&src, &dst);
    assert!(result.is_ok(), "copy of empty dir should succeed");
    assert!(dst.exists(), "destination should exist");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn copy_dir_recursive_handles_nonexistent_src() {
    let dir = std::env::temp_dir().join(format!("enclave-snap-nosrc-{}", std::process::id()));
    let src = dir.join("does-not-exist");
    let dst = dir.join("dst");

    let result = copy_dir_recursive(&src, &dst);
    assert!(
        result.is_ok(),
        "copy of non-existent src should succeed by creating dst"
    );
    assert!(dst.exists(), "destination should be created");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn copy_dir_recursive_copies_nested_files() {
    let dir = std::env::temp_dir().join(format!("enclave-snap-nested-{}", std::process::id()));
    let src = dir.join("src");
    let dst = dir.join("dst");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("file.txt"), "hello").unwrap();
    fs::write(src.join("sub/nested.txt"), "world").unwrap();

    let result = copy_dir_recursive(&src, &dst);
    assert!(result.is_ok());
    assert_eq!(fs::read_to_string(dst.join("file.txt")).unwrap(), "hello");
    assert_eq!(
        fs::read_to_string(dst.join("sub/nested.txt")).unwrap(),
        "world"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn reset_path_creates_fresh_dir() {
    let dir = std::env::temp_dir().join(format!("enclave-snap-reset-{}", std::process::id()));
    let target = dir.join("target");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("old.txt"), "old").unwrap();

    reset_path(&target).unwrap();

    assert!(target.exists());
    assert!(
        fs::read_dir(&target).unwrap().count() == 0,
        "reset directory should be empty"
    );
    let _ = fs::remove_dir_all(&dir);
}
