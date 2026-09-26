use std::fs;
use std::path::PathBuf;

use super::*;

fn fixture_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("enclave-logs-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("create fixture dir");
    dir
}

fn stream_id_of(path: &Path) -> String {
    log_stream_id(&fs::metadata(path).expect("stat log"))
}

#[test]
fn header_fields_escape_control_characters() {
    assert_eq!(
        sanitize_log_header_field("cmd\n--flag\tvalue"),
        "cmd\\n--flag\\tvalue"
    );
}

#[test]
fn delta_read_returns_only_the_appended_bytes() {
    let dir = fixture_dir();
    let path = dir.join("session.log");
    fs::write(&path, b"abcdef").expect("write log");
    let stream_id = stream_id_of(&path);

    let delta = read_log_delta(&path, 3, Some(&stream_id)).expect("read delta");
    assert_eq!(delta.content, "def");
    assert_eq!(delta.next_offset, 6);
    assert!(!delta.reset);
    assert_eq!(delta.stream_id.as_deref(), Some(stream_id.as_str()));

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn delta_read_resets_when_the_log_is_replaced() {
    let dir = fixture_dir();
    let path = dir.join("session.log");
    fs::write(&path, b"aaaa").expect("write log");
    let stream_id = stream_id_of(&path);

    // A replacement is longer than the offset the follower holds, so a length
    // check alone would silently return a slice of unrelated bytes.
    let replacement = dir.join("session.log.new");
    fs::write(&replacement, b"bbbbbbbb").expect("write replacement");
    fs::rename(&replacement, &path).expect("replace log");
    assert_ne!(stream_id_of(&path), stream_id);

    let delta = read_log_delta(&path, 4, Some(&stream_id)).expect("read delta");
    assert!(delta.reset, "a replaced log must reset the follower");
    assert_eq!(delta.content, "bbbbbbbb");
    assert_eq!(delta.next_offset, 8);

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn delta_read_resets_when_the_log_is_truncated() {
    let dir = fixture_dir();
    let path = dir.join("session.log");
    fs::write(&path, b"abcdef").expect("write log");
    let stream_id = stream_id_of(&path);
    fs::write(&path, b"a").expect("truncate log");

    let delta = read_log_delta(&path, 6, Some(&stream_id)).expect("read delta");
    assert!(delta.reset, "a shortened log must reset the follower");
    assert_eq!(delta.content, "a");
    assert_eq!(delta.next_offset, 1);

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn tail_read_reports_the_end_offset_and_truncation() {
    let dir = fixture_dir();
    let path = dir.join("session.log");
    fs::write(&path, b"0123456789").expect("write log");

    let tail = read_tail_bytes(&path, 4).expect("read tail");
    assert!(tail.truncated);
    assert_eq!(tail.content, "6789");
    assert_eq!(tail.end_offset, 10);
    assert_eq!(tail.stream_id, stream_id_of(&path));

    let full = read_tail_bytes(&path, 100).expect("read whole log");
    assert!(!full.truncated);
    assert_eq!(full.content, "0123456789");
    assert_eq!(full.end_offset, 10);

    let _ = fs::remove_dir_all(dir);
}

/// A follower must not be handed an unbounded slice of a fast-growing log. The
/// response cap is enforced by the client, and exceeding it turns a readable log
/// into a hard error, so a poll returns a bounded chunk and the follower asks for
/// the rest.
#[test]
fn a_large_delta_is_returned_in_bounded_chunks() {
    let dir = fixture_dir();
    let path = dir.join("session.log");
    let total = MAX_LOG_DELTA_BYTES + 4096;
    fs::write(&path, vec![b'x'; total as usize]).expect("write log");
    let stream_id = stream_id_of(&path);

    let first = read_log_delta(&path, 0, Some(&stream_id)).expect("first delta");
    assert_eq!(first.content.len() as u64, MAX_LOG_DELTA_BYTES);
    assert_eq!(first.next_offset, MAX_LOG_DELTA_BYTES);
    assert!(!first.reset, "a bounded chunk is not a reset");
    assert!(first.has_more, "the rest of the log is still waiting");

    // The follower resumes where the chunk ended and gets the remainder.
    let second = read_log_delta(&path, first.next_offset, Some(&stream_id)).expect("second delta");
    assert_eq!(second.content.len() as u64, total - MAX_LOG_DELTA_BYTES);
    assert_eq!(second.next_offset, total);
    assert!(!second.has_more, "the follower has caught up");

    // And then nothing, rather than repeating the tail.
    let third = read_log_delta(&path, second.next_offset, Some(&stream_id)).expect("third delta");
    assert!(third.content.is_empty());
    assert_eq!(third.next_offset, total);

    let _ = fs::remove_dir_all(dir);
}
