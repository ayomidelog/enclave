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

#[test]
fn is_mountpoint_uses_mountinfo_without_mountpoint_process() {
    assert!(is_mountpoint(Path::new("/proc")).expect("read mountinfo"));
    assert!(!is_mountpoint(Path::new("/tmp/enclave-no-such-mount")).expect("read mountinfo"));
}

#[test]
fn mountinfo_snapshot_orders_nested_mounts_deepest_first() {
    let snapshot = MountInfoSnapshot::parse(concat!(
        "100 1 0:50 / /tmp/enclave/ws/fs rw,relatime - ext4 /dev/loop0 rw\n",
        "101 100 0:51 / /tmp/enclave/ws/fs/cache\\040data rw,relatime - tmpfs tmpfs rw\n"
    ));
    assert_eq!(
        snapshot.at_or_below(Path::new("/tmp/enclave/ws/fs")),
        vec![
            PathBuf::from("/tmp/enclave/ws/fs/cache data"),
            PathBuf::from("/tmp/enclave/ws/fs")
        ]
    );
    assert!(snapshot.contains(Path::new("/tmp/enclave/ws/fs")));
}

#[test]
fn reflink_copy_file_preserves_content_or_reports_unsupported_filesystem() {
    let dir = std::env::temp_dir().join(format!("enclave-fsutil-reflink-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source");
    let destination = dir.join("destination");
    fs::write(&source, b"reflink benchmark fixture").unwrap();

    let cloned = reflink_copy_file(&source, &destination).unwrap();
    if !cloned {
        fs::copy(&source, &destination).unwrap();
    }
    assert_eq!(
        fs::read(&destination).unwrap(),
        b"reflink benchmark fixture"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn copy_file_range_file_preserves_content_or_reports_unsupported_filesystem() {
    let dir =
        std::env::temp_dir().join(format!("enclave-fsutil-copy-range-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source");
    let destination = dir.join("destination");
    fs::write(&source, b"copy file range fixture").unwrap();

    let copied = copy_file_range_file(&source, &destination).unwrap();
    if !copied {
        fs::copy(&source, &destination).unwrap();
    }
    assert_eq!(fs::read(&destination).unwrap(), b"copy file range fixture");
    let _ = fs::remove_dir_all(dir);
}
