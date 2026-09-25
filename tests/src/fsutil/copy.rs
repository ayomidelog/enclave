//! Copying a file by reflink or by range, and falling back when neither is
//! available.

use super::*;

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
