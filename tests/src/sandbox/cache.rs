//! The rootfs cache index: what it records, and what it answers.

use crate::sandbox::cache::{ensure, evaluate, index_path, rebuild, register, CacheVerdict};
use std::fs;
use std::path::PathBuf;

/// Whether a cached rootfs can be reused. The tests assert on the reason as
/// well, so this goes through the same check the daemon does.
fn contains(cache_root: &std::path::Path, key: &str, path: &std::path::Path) -> bool {
    evaluate(cache_root, key, path).is_hit()
}

fn temporary_cache() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "enclave-rootfs-cache-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(path.join("suite").join("bin")).unwrap();
    fs::create_dir_all(path.join("suite").join("etc")).unwrap();
    fs::create_dir_all(path.join("suite").join("usr")).unwrap();
    path
}

#[test]
fn index_tracks_and_invalidates_cache_fingerprint() {
    let cache_root = temporary_cache();
    let cache_path = cache_root.join("suite");
    rebuild(&cache_root).unwrap();
    assert!(contains(&cache_root, "suite", &cache_path));
    fs::remove_dir_all(cache_path.join("usr")).unwrap();
    assert!(!contains(&cache_root, "suite", &cache_path));
    let _ = fs::remove_dir_all(cache_root);
}

#[test]
fn register_replaces_existing_key() {
    let cache_root = temporary_cache();
    let first_path = cache_root.join("suite");
    let second_path = cache_root.join("other");
    for directory in ["bin", "etc", "usr"] {
        fs::create_dir_all(second_path.join(directory)).unwrap();
    }
    rebuild(&cache_root).unwrap();
    register(&cache_root, "suite", &second_path).unwrap();
    assert!(!contains(&cache_root, "suite", &first_path));
    assert!(contains(&cache_root, "suite", &second_path));
    let _ = fs::remove_dir_all(cache_root);
}

#[test]
fn index_records_provenance_and_content_digest() {
    let cache_root = temporary_cache();
    rebuild(&cache_root).unwrap();
    let raw = fs::read(index_path(&cache_root)).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    let entry = &json["entries"][0];
    assert_eq!(entry["suite"], "suite");
    assert_eq!(entry["architecture"], std::env::consts::ARCH);
    assert!(!entry["source"].as_str().unwrap().is_empty());
    assert!(!entry["created_at"].as_str().unwrap().is_empty());
    assert_eq!(entry["content_digest"].as_str().unwrap().len(), 16);
    assert_eq!(entry["tool_version"], env!("CARGO_PKG_VERSION"));
    let _ = fs::remove_dir_all(cache_root);
}

#[test]
fn ensure_adopts_directories_missing_from_the_index() {
    let cache_root = temporary_cache();
    // Build an index that only knows about one suite, then add a second
    // rootfs directory the way an operator extracting a tarball would.
    rebuild(&cache_root).unwrap();
    let manual = cache_root.join("manual");
    for directory in ["bin", "etc", "usr"] {
        fs::create_dir_all(manual.join(directory)).unwrap();
    }
    assert!(!contains(&cache_root, "manual", &manual));

    ensure(&cache_root).unwrap();

    assert!(contains(&cache_root, "manual", &manual));
    assert!(contains(&cache_root, "suite", &cache_root.join("suite")));
    let _ = fs::remove_dir_all(cache_root);
}
/// Every way a cache can miss is reported as itself.
///
/// The question an operator asks when a sandbox copies a rootfs they expected
/// it to share is which check failed, and each answer means something
/// different: a cache that was never built, one that was moved, one whose
/// directory was deleted, or one whose contents were edited underneath it.
/// Collapsing them into one "cache miss" would leave that question
/// unanswerable.
#[test]
fn every_cache_verdict_names_what_it_found() {
    let cache_root = temporary_cache();
    let cache_path = cache_root.join("suite");

    // An index that is readable but holds no entry for this rootfs is the
    // shape a cache has before anything was ever registered in it.
    fs::write(index_path(&cache_root), br#"{"version":1,"entries":[]}"#).unwrap();
    assert_eq!(
        evaluate(&cache_root, "suite", &cache_path),
        CacheVerdict::NotRegistered
    );

    rebuild(&cache_root).unwrap();
    assert_eq!(
        evaluate(&cache_root, "suite", &cache_path),
        CacheVerdict::Hit
    );

    // A directory the fingerprint covers is removed, which is the shape a
    // half-finished extraction leaves behind.
    fs::remove_dir_all(cache_path.join("usr")).unwrap();
    assert_eq!(
        evaluate(&cache_root, "suite", &cache_path),
        CacheVerdict::MissingRequiredDirectories
    );

    // Put the directory back and move its timestamp instead, so the required
    // directories are all present and only the recorded metadata disagrees. The
    // timestamp is set explicitly rather than by writing a file: a recreated
    // directory can land on the inode it just freed and a write inside one can
    // fall in the same clock tick, either of which would leave the fingerprint
    // unchanged and make this assertion pass or fail on timing.
    fs::create_dir_all(cache_path.join("usr")).unwrap();
    set_directory_mtime_seconds_ago(&cache_path.join("usr"), 3600);
    assert_eq!(
        evaluate(&cache_root, "suite", &cache_path),
        CacheVerdict::ContentsChanged
    );

    // An index that cannot be parsed is its own answer rather than a miss
    // that looks like an empty cache.
    fs::write(index_path(&cache_root), b"not json").unwrap();
    assert_eq!(
        evaluate(&cache_root, "suite", &cache_path),
        CacheVerdict::IndexUnreadable
    );

    // Every verdict is a phrase rather than an empty string, so the log line
    // and any report that carries one is readable.
    for verdict in [
        CacheVerdict::Hit,
        CacheVerdict::IndexUnreadable,
        CacheVerdict::NotRegistered,
        CacheVerdict::MissingRequiredDirectories,
        CacheVerdict::ContentsChanged,
    ] {
        assert!(verdict.describe().len() > 20, "{verdict:?}");
    }

    let _ = fs::remove_dir_all(cache_root);
}

/// Set a directory's modification time to `seconds_ago` in the past.
///
/// The cache fingerprint covers the required directories' metadata, so this is
/// how a test changes exactly that and nothing else.
fn set_directory_mtime_seconds_ago(path: &std::path::Path, seconds_ago: i64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the unix epoch");
    let seconds = now.as_secs() as i64 - seconds_ago;
    let times = [
        libc::timespec {
            tv_sec: seconds,
            tv_nsec: 0,
        },
        libc::timespec {
            tv_sec: seconds,
            tv_nsec: 0,
        },
    ];
    let path_c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    let result = unsafe { libc::utimensat(libc::AT_FDCWD, path_c.as_ptr(), times.as_ptr(), 0) };
    assert_eq!(result, 0, "failed to set the mtime of {}", path.display());
}
