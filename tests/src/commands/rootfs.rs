use std::fs;
use std::path::PathBuf;

use super::{
    file_name_from_url, locate_extracted_rootfs, output_uses_gzip, resolve_cache_target,
    temporary_workspace,
};

#[test]
fn resolve_cache_target_accepts_suite() {
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-rootfs-suite-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let target =
        resolve_cache_target(&state_dir, Some("bookworm"), false).expect("resolve suite target");
    assert_eq!(target.label, "bookworm");
    assert!(target
        .cache_path
        .ends_with("sandboxes/rootfs-cache/bookworm"));
    let _ = fs::remove_dir_all(state_dir);
}

#[test]
fn resolve_cache_target_accepts_base() {
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-rootfs-base-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let target = resolve_cache_target(&state_dir, None, true).expect("resolve base target");
    assert_eq!(target.label, "base");
    assert!(target.cache_path.ends_with("sandboxes/rootfs-cache/base"));
    let _ = fs::remove_dir_all(state_dir);
}

#[test]
fn resolve_cache_target_requires_exactly_one_selector() {
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-rootfs-select-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    assert!(resolve_cache_target(&state_dir, None, false).is_err());
    assert!(resolve_cache_target(&state_dir, Some("bookworm"), true).is_err());
    let _ = fs::remove_dir_all(state_dir);
}

#[test]
fn locate_extracted_rootfs_accepts_archive_root_layout() {
    let dir = temporary_workspace("rootfs-layout-root");
    fs::create_dir_all(dir.join("bin")).expect("create bin");
    fs::create_dir_all(dir.join("etc")).expect("create etc");
    fs::create_dir_all(dir.join("usr")).expect("create usr");
    let located = locate_extracted_rootfs(&dir).expect("locate rootfs");
    assert_eq!(located, dir);
    let _ = fs::remove_dir_all(located);
}

#[test]
fn locate_extracted_rootfs_accepts_single_nested_layout() {
    let dir = temporary_workspace("rootfs-layout-nested");
    let nested = dir.join("bookworm");
    fs::create_dir_all(nested.join("bin")).expect("create bin");
    fs::create_dir_all(nested.join("etc")).expect("create etc");
    fs::create_dir_all(nested.join("usr")).expect("create usr");
    let located = locate_extracted_rootfs(&dir).expect("locate nested rootfs");
    assert_eq!(located, nested);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn output_uses_gzip_detects_gzip_extensions() {
    assert!(output_uses_gzip(&PathBuf::from("rootfs.tar.gz")));
    assert!(output_uses_gzip(&PathBuf::from("rootfs.tgz")));
    assert!(!output_uses_gzip(&PathBuf::from("rootfs.tar")));
}

#[test]
fn file_name_from_url_uses_last_path_component() {
    assert_eq!(
        file_name_from_url("https://example.com/releases/bookworm-rootfs.tar.gz"),
        "bookworm-rootfs.tar.gz"
    );
    assert_eq!(
        file_name_from_url("https://example.com/releases/"),
        "releases"
    );
}
