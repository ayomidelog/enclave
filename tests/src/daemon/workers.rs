use super::request::{run_request, RequestOutcome};
use super::{classify_stream, peek_workspace_key, ExecQueue, RequestClass, RequestJob};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

#[test]
fn classifies_workspace_copy_without_consuming_request() {
    let (mut writer, reader) = UnixStream::pair().unwrap();
    writer
        .write_all(b"{\"action\":\"workspace.cp\",\"params\":{}}\n")
        .unwrap();
    assert_eq!(classify_stream(&reader), RequestClass::Transfer);
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
        .write_all(b"{\"action\":\"workspace.list\",\"params\":{}}\n")
        .unwrap();
    assert_eq!(classify_stream(&reader), RequestClass::Control);
}

#[test]
fn a_command_that_mentions_workspace_exec_does_not_change_the_pool() {
    let (mut writer, reader) = UnixStream::pair().unwrap();
    writer
        .write_all(
            br#"{"action":"workspace.list","params":{"command":["echo workspace.exec"]}}
"#,
        )
        .unwrap();
    assert_eq!(classify_stream(&reader), RequestClass::Control);
}

#[test]
fn a_request_split_across_writes_is_still_classified() {
    let (mut writer, reader) = UnixStream::pair().unwrap();
    writer.write_all(b"{\"action\":\"workspace.").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    writer.write_all(b"exec\",\"params\":{}}\n").unwrap();
    assert_eq!(classify_stream(&reader), RequestClass::Exec);
}

#[test]
fn classifies_workspace_exec_into_its_own_pool() {
    // The wedge this prevents: six commands that each wait on a workspace helper
    // occupied all six control workers, so `ping` timed out while the daemon was
    // healthy, and the caller reaped it. A command must never hold a control slot.
    let (mut writer, reader) = UnixStream::pair().unwrap();
    writer
        .write_all(b"{\"action\":\"workspace.exec\",\"params\":{}}\n")
        .unwrap();
    assert_eq!(classify_stream(&reader), RequestClass::Exec);
}

#[test]
fn classifies_sandbox_exec_setup_into_the_exec_pool() {
    let (mut writer, reader) = UnixStream::pair().unwrap();
    writer
        .write_all(b"{\"action\":\"sandbox.exec_setup\",\"params\":{}}\n")
        .unwrap();
    assert_eq!(classify_stream(&reader), RequestClass::Exec);
}

#[test]
fn an_empty_socket_is_classified_as_control() {
    // Nothing has been written yet, so there is no payload to inspect. Control is
    // the safe default: it is the pool whose requests do not depend on a large body.
    let (_writer, reader) = UnixStream::pair().unwrap();
    assert_eq!(classify_stream(&reader), RequestClass::Control);
}

#[test]
fn an_exec_request_reports_the_workspace_it_targets() {
    // The pool gives each workspace a bounded share of exec workers, which needs
    // the target workspace before the request is dispatched.
    let (mut writer, reader) = UnixStream::pair().unwrap();
    writer
        .write_all(b"{\"action\":\"workspace.exec\",\"params\":{\"workspace\":\"session-abc\",\"command\":[\"ls\"]}}\n")
        .unwrap();
    assert_eq!(peek_workspace_key(&reader).as_deref(), Some("session-abc"));
    // The peek must not consume the request the worker still has to read.
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
    assert!(String::from_utf8_lossy(&buffer[..size as usize]).contains("session-abc"));
}

#[test]
fn a_workspace_id_field_is_recognised_too() {
    let (mut writer, reader) = UnixStream::pair().unwrap();
    writer
        .write_all(
            b"{\"action\":\"workspace.exec\",\"params\":{\"workspace_id\":\"session-xyz\"}}\n",
        )
        .unwrap();
    assert_eq!(peek_workspace_key(&reader).as_deref(), Some("session-xyz"));
}

#[test]
fn a_request_without_a_workspace_has_no_key() {
    let (mut writer, reader) = UnixStream::pair().unwrap();
    writer
        .write_all(b"{\"action\":\"workspace.exec\",\"params\":{}}\n")
        .unwrap();
    assert_eq!(peek_workspace_key(&reader), None);
}

#[test]
fn one_workspace_cannot_fill_the_exec_queue() {
    let queue = ExecQueue::new(1);
    let (first, _first_peer) = UnixStream::pair().unwrap();
    let (second, _second_peer) = UnixStream::pair().unwrap();
    let (third, _third_peer) = UnixStream::pair().unwrap();
    for (stream, key) in [(first, "busy"), (second, "busy"), (third, "other")] {
        queue
            .enqueue(
                RequestJob {
                    stream,
                    workspace_key: Some(key.to_string()),
                },
                8,
            )
            .ok()
            .expect("queue a request");
    }

    let first = queue.next().expect("first request");
    assert_eq!(first.workspace_key.as_deref(), Some("busy"));
    // A second command for `busy` waits behind the first instead of parking a
    // worker on the workspace's single-command helper.
    let second = queue.next().expect("second request");
    assert_eq!(second.workspace_key.as_deref(), Some("other"));
    queue.finish(first.workspace_key.as_deref());
    let third = queue.next().expect("pending busy request");
    assert_eq!(third.workspace_key.as_deref(), Some("busy"));
    queue.finish(third.workspace_key.as_deref());
    queue.finish(second.workspace_key.as_deref());
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

#[test]
fn one_workspace_admits_one_exec_job_at_a_time() {
    // The persistent helper runs one command per workspace. Admitting two only
    // parks the second worker on the helper mutex while another workspace waits.
    let queue = ExecQueue::new(super::EXEC_PER_WORKSPACE);
    let (first, _first_peer) = UnixStream::pair().unwrap();
    let (second, _second_peer) = UnixStream::pair().unwrap();
    for stream in [first, second] {
        queue
            .enqueue(
                RequestJob {
                    stream,
                    workspace_key: Some("busy".to_string()),
                },
                8,
            )
            .ok()
            .expect("queue a request");
    }
    let first = queue.next().expect("first request");
    assert_eq!(first.workspace_key.as_deref(), Some("busy"));
    queue.finish(first.workspace_key.as_deref());
    let second = queue.next().expect("queued request");
    assert_eq!(second.workspace_key.as_deref(), Some("busy"));
    queue.finish(second.workspace_key.as_deref());
}

#[test]
fn a_disconnected_client_does_not_consume_the_exec_queue() {
    let queue = ExecQueue::new(1);
    let (active, _active_peer) = UnixStream::pair().unwrap();
    queue
        .enqueue(
            RequestJob {
                stream: active,
                workspace_key: Some("busy".to_string()),
            },
            2,
        )
        .ok()
        .expect("queue the active request");
    let active = queue.next().expect("active request");

    let (abandoned, abandoned_peer) = UnixStream::pair().unwrap();
    queue
        .enqueue(
            RequestJob {
                stream: abandoned,
                workspace_key: Some("busy".to_string()),
            },
            2,
        )
        .ok()
        .expect("queue the abandoned request");
    drop(abandoned_peer);

    let (replacement, _replacement_peer) = UnixStream::pair().unwrap();
    queue
        .enqueue(
            RequestJob {
                stream: replacement,
                workspace_key: Some("other".to_string()),
            },
            2,
        )
        .ok()
        .expect("a disconnected request must not fill the queue");

    queue.finish(active.workspace_key.as_deref());
    let replacement = queue.next().expect("replacement request");
    assert_eq!(replacement.workspace_key.as_deref(), Some("other"));
    queue.finish(replacement.workspace_key.as_deref());
}
