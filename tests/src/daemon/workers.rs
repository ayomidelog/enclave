use super::request::{run_request, RequestOutcome};
use super::{configured_worker_count, stream_is_transfer};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

#[test]
fn classifies_workspace_copy_without_consuming_request() {
    let (mut writer, reader) = UnixStream::pair().unwrap();
    writer
        .write_all(br#"{"action":"workspace.cp","params":{}}\n"#)
        .unwrap();
    assert!(stream_is_transfer(&reader));
    let mut buffer = [0u8; 128];
    let size = unsafe {
        libc::recv(
            reader.as_raw_fd(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            libc::MSG_PEEK,
        )
    };
    assert!(size > 0);
    assert!(String::from_utf8_lossy(&buffer[..size as usize]).contains("workspace.cp"));
}

#[test]
fn classifies_control_request_separately() {
    let (mut writer, reader) = UnixStream::pair().unwrap();
    writer
        .write_all(br#"{"action":"workspace.list","params":{}}\n"#)
        .unwrap();
    assert!(!stream_is_transfer(&reader));
}

#[test]
fn worker_count_rejects_unbounded_and_invalid_values() {
    unsafe { std::env::set_var("ENCLAVE_TEST_WORKER_COUNT", "0") };
    assert_eq!(configured_worker_count("ENCLAVE_TEST_WORKER_COUNT", 6), 6);
    unsafe { std::env::set_var("ENCLAVE_TEST_WORKER_COUNT", "65") };
    assert_eq!(configured_worker_count("ENCLAVE_TEST_WORKER_COUNT", 6), 6);
    unsafe { std::env::set_var("ENCLAVE_TEST_WORKER_COUNT", "3") };
    assert_eq!(configured_worker_count("ENCLAVE_TEST_WORKER_COUNT", 6), 3);
    unsafe { std::env::remove_var("ENCLAVE_TEST_WORKER_COUNT") };
}

#[test]
fn a_panicking_request_is_reported_and_does_not_escape() {
    // The daemon's control pool is six threads. If a panic escaped here the
    // thread would die, and six panics would leave the daemon running and
    // accepting connections while nothing answered a lifecycle request.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = run_request(|| panic!("simulated request panic"));
    std::panic::set_hook(previous);
    assert_eq!(
        outcome,
        RequestOutcome::Panicked("simulated request panic".to_string())
    );
}

#[test]
fn a_request_error_and_a_success_are_reported_separately() {
    assert_eq!(run_request(|| Ok(())), RequestOutcome::Finished);
    let outcome = run_request(|| Err(anyhow::anyhow!("disk full")));
    assert_eq!(outcome, RequestOutcome::Failed("disk full".to_string()));
}

#[test]
fn a_worker_keeps_serving_after_a_panicking_request() {
    // The loop the worker runs is this sequence, so the second request after a
    // panic is what proves the worker is still usable.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let first = run_request(|| panic!("boom"));
    let second = run_request(|| Ok(()));
    std::panic::set_hook(previous);
    assert!(matches!(first, RequestOutcome::Panicked(_)));
    assert_eq!(second, RequestOutcome::Finished);
}
