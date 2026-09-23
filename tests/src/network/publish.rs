use super::*;

#[test]
fn connection_limiter_rejects_above_configured_capacity() {
    let limiter = Arc::new(ConnectionLimiter::new(1));
    let first = limiter.try_acquire().expect("first permit");
    assert!(limiter.try_acquire().is_none());
    assert_eq!(limiter.active(), 1);
    drop(first);
    assert_eq!(limiter.active(), 0);
    assert!(limiter.try_acquire().is_some());
}

#[test]
fn copy_until_shutdown_stops_without_waiting_for_eof() {
    let shutdown = AtomicBool::new(true);
    let mut reader = std::io::Cursor::new(vec![1u8, 2, 3]);
    let mut output = Vec::new();
    copy_until_shutdown(&mut reader, &mut output, &shutdown).expect("copy");
    assert!(output.is_empty());
}

#[test]
fn copy_until_shutdown_copies_available_data() {
    let shutdown = AtomicBool::new(false);
    let mut reader = std::io::Cursor::new(vec![1u8, 2, 3]);
    let mut output = Vec::new();
    copy_until_shutdown(&mut reader, &mut output, &shutdown).expect("copy");
    assert_eq!(output, vec![1, 2, 3]);
}
