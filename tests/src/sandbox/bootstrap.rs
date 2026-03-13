use super::*;
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::process::Command;

#[test]
fn copy_dir_recursive_copies_files() {
    let tmp = std::env::temp_dir().join(format!("enclave-bootstrap-test-{}", std::process::id()));
    let src = tmp.join("src");
    let dst = tmp.join("dst");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("file.txt"), "hello").unwrap();
    fs::write(src.join("sub").join("nested.txt"), "world").unwrap();

    copy_dir_recursive(&src, &dst).unwrap();

    assert_eq!(fs::read_to_string(dst.join("file.txt")).unwrap(), "hello");
    assert_eq!(
        fs::read_to_string(dst.join("sub").join("nested.txt")).unwrap(),
        "world"
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn copy_dir_recursive_preserves_absolute_symlink_targets() {
    let tmp =
        std::env::temp_dir().join(format!("enclave-bootstrap-linktest-{}", std::process::id()));
    let src = tmp.join("src");
    let dst = tmp.join("dst");
    if tmp.exists() {
        fs::remove_dir_all(&tmp).expect("failed to cleanup pre-existing temp test dir");
    }
    fs::create_dir_all(&src).unwrap();
    std::os::unix::fs::symlink("/usr/lib", src.join("lib")).unwrap();

    copy_dir_recursive(&src, &dst).unwrap();
    let target = fs::read_link(dst.join("lib")).expect("read copied symlink");
    assert_eq!(target, std::path::PathBuf::from("/usr/lib"));

    if tmp.exists() {
        fs::remove_dir_all(&tmp).expect("failed to cleanup temp test dir");
    }
}

#[test]
fn copy_dir_recursive_preserves_fifo_entries() {
    let tmp = std::env::temp_dir().join(format!("enclave-bootstrap-fifo-{}", std::process::id()));
    let src = tmp.join("src");
    let dst = tmp.join("dst");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&src).unwrap();

    let fifo = src.join("named-pipe");
    let status = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo should succeed");

    copy_dir_recursive(&src, &dst).unwrap();

    let metadata = fs::symlink_metadata(dst.join("named-pipe")).expect("fifo metadata");
    assert!(
        metadata.file_type().is_fifo(),
        "copied entry should remain a fifo"
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn cached_rootfs_fails_when_cache_missing() {
    let tmp =
        std::env::temp_dir().join(format!("enclave-bootstrap-nocache-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let state_dir = tmp.join("state");
    fs::create_dir_all(tmp.join("rootfs")).unwrap();

    let result = bootstrap_cached_rootfs(&tmp.join("rootfs"), "test", "bookworm", &state_dir);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("cached rootfs not found"), "got: {}", msg);

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn rootfs_cache_dir_is_under_sandboxes() {
    let state_dir = Path::new("/var/lib/enclave");
    let cache = rootfs_cache_dir(state_dir);
    assert_eq!(
        cache,
        PathBuf::from("/var/lib/enclave/sandboxes/rootfs-cache")
    );
}

#[test]
fn has_rootfs_content_detects_empty_dir() {
    let tmp = std::env::temp_dir().join(format!("enclave-rootfs-content-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    assert!(!has_rootfs_content(&tmp));

    fs::create_dir_all(tmp.join("bin")).unwrap();
    fs::create_dir_all(tmp.join("etc")).unwrap();
    assert!(!has_rootfs_content(&tmp));
    fs::create_dir_all(tmp.join("usr")).unwrap();
    assert!(has_rootfs_content(&tmp));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn has_rootfs_content_returns_false_for_missing() {
    let tmp = std::env::temp_dir().join("enclave-rootfs-missing-nonexistent");
    assert!(!has_rootfs_content(&tmp));
}

#[test]
fn has_rootfs_content_requires_all_expected_dirs() {
    let tmp = std::env::temp_dir().join(format!("enclave-rootfs-required-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("bin")).unwrap();
    assert!(!has_rootfs_content(&tmp));
    fs::create_dir_all(tmp.join("etc")).unwrap();
    assert!(!has_rootfs_content(&tmp));
    fs::create_dir_all(tmp.join("usr")).unwrap();
    assert!(has_rootfs_content(&tmp));
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn cache_rootfs_suite_saves_and_detects() {
    let tmp = std::env::temp_dir().join(format!("enclave-cache-suite-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let state_dir = tmp.join("state");
    let rootfs = tmp.join("rootfs");
    fs::create_dir_all(rootfs.join("bin")).unwrap();
    fs::create_dir_all(rootfs.join("etc")).unwrap();
    fs::create_dir_all(rootfs.join("usr")).unwrap();
    fs::write(rootfs.join("usr").join("test.txt"), "cached").unwrap();

    cache_rootfs_suite(&rootfs, &state_dir, "bookworm").unwrap();

    let cache = rootfs_cache_dir(&state_dir).join("bookworm");
    assert!(cache.is_dir());
    assert!(has_rootfs_content(&cache));
    assert_eq!(
        fs::read_to_string(cache.join("usr").join("test.txt")).unwrap(),
        "cached"
    );

    cache_rootfs_suite(&rootfs, &state_dir, "bookworm").unwrap();

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn cached_rootfs_uses_suite_keyed_cache() {
    let tmp = std::env::temp_dir().join(format!("enclave-suite-cache-hit-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let state_dir = tmp.join("state");
    let rootfs_out = tmp.join("rootfs");
    fs::create_dir_all(&rootfs_out).unwrap();

    let cache = rootfs_cache_dir(&state_dir).join("bookworm");
    fs::create_dir_all(cache.join("bin")).unwrap();
    fs::create_dir_all(cache.join("etc")).unwrap();
    fs::create_dir_all(cache.join("usr")).unwrap();
    fs::write(cache.join("bin").join("sh"), "#!/bin/sh").unwrap();

    bootstrap_cached_rootfs(&rootfs_out, "test", "bookworm", &state_dir).unwrap();

    assert_eq!(
        fs::read_to_string(rootfs_out.join("bin").join("sh")).unwrap(),
        "#!/bin/sh"
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn cached_rootfs_falls_back_to_generic_base() {
    let tmp =
        std::env::temp_dir().join(format!("enclave-generic-cache-hit-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let state_dir = tmp.join("state");
    let rootfs_out = tmp.join("rootfs");
    fs::create_dir_all(&rootfs_out).unwrap();

    let cache = rootfs_cache_dir(&state_dir).join("base");
    fs::create_dir_all(cache.join("bin")).unwrap();
    fs::create_dir_all(cache.join("etc")).unwrap();
    fs::create_dir_all(cache.join("usr")).unwrap();
    fs::write(cache.join("etc").join("hostname"), "generic").unwrap();

    bootstrap_cached_rootfs(&rootfs_out, "test", "unknown-suite", &state_dir).unwrap();

    assert_eq!(
        fs::read_to_string(rootfs_out.join("etc").join("hostname")).unwrap(),
        "generic"
    );

    let _ = fs::remove_dir_all(&tmp);
}
