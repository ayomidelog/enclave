use super::*;

#[test]
fn write_file_atomic_creates_file_with_correct_content() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-test-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.txt");

    write_file_atomic(&path, b"hello world", 0o600).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "hello world");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn write_file_atomic_sets_permissions() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-perms-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("perms.txt");

    write_file_atomic(&path, b"content", 0o600).unwrap();

    let metadata = fs::metadata(&path).unwrap();
    let mode = metadata.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn write_file_atomic_overwrites_existing() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-overwrite-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("overwrite.txt");

    write_file_atomic(&path, b"first", 0o600).unwrap();
    write_file_atomic(&path, b"second", 0o600).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "second");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn with_file_lock_executes_operation() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-lock-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let lock_path = dir.join("test.lock");

    let result = with_file_lock(&lock_path, || Ok(42)).unwrap();
    assert_eq!(result, 42);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn with_file_lock_propagates_errors() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-lockerr-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let lock_path = dir.join("test.lock");

    let result: Result<()> = with_file_lock(&lock_path, || anyhow::bail!("intentional error"));
    assert!(result.is_err());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ensure_secure_dir_creates_and_validates_directory() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-secure-{}", std::process::id()));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }

    ensure_secure_dir(&dir).unwrap();

    assert!(dir.is_dir());
    let metadata = fs::metadata(&dir).unwrap();
    let mode = metadata.permissions().mode() & 0o777;
    assert_eq!(
        mode & 0o022,
        0,
        "directory should not be group/world writable"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ensure_secure_dir_rejects_sticky_world_writable_mode() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-sticky-{}", std::process::id()));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o1777)).unwrap();

    let err = ensure_secure_dir(&dir).unwrap_err();
    assert!(
        err.to_string()
            .contains("use a private subdirectory (0700) instead"),
        "unexpected error: {err:#}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ensure_path_within_rejects_traversal() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-within-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();

    let result = ensure_path_within(&dir, &PathBuf::from("../../etc/passwd"), "test_path");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("escapes base directory"), "got: {}", msg);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ensure_path_within_accepts_valid_subpath() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-within-ok-{}", std::process::id()));
    fs::create_dir_all(dir.join("sub")).unwrap();

    let result = ensure_path_within(&dir, &PathBuf::from("sub/file.txt"), "test_path");
    assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn temporary_path_for_generates_unique_paths() {
    let path = PathBuf::from("/tmp/myfile.txt");
    let tmp1 = temporary_path_for(&path);
    let tmp2 = temporary_path_for(&path);
    assert_ne!(tmp1, tmp2, "temporary paths should be unique");
    assert!(tmp1.to_string_lossy().contains(".myfile.txt.tmp."));
}
