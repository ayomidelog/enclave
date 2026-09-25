//! One record: its phases, its terminal state, and its identity.

use super::*;

/// A journal is usable the moment it begins.
///
/// The record and its starting phase are written together, so a crash after
/// begin returns cannot leave a journal that exists but says nothing.
#[test]
fn begin_records_the_starting_phase_in_one_step() {
    let state =
        std::env::temp_dir().join(format!("enclave-operation-begin-{}", uuid::Uuid::new_v4()));
    let journal = Journal::begin(&state, "workspace.start", "sb/ws").expect("begin journal");
    let id = journal.id().to_string();

    let on_disk = load(&state, &id).expect("journal is readable immediately after begin");
    assert_eq!(on_disk.status, OperationStatus::Running);
    assert_eq!(on_disk.phase, "starting");

    fs::remove_dir_all(state).expect("remove journal fixture");
}

/// A phase update is a complete record on disk even though it is written
/// without fsync, so a reader never sees a half-written journal.
#[test]
fn phase_updates_are_atomic_and_readable() {
    let state =
        std::env::temp_dir().join(format!("enclave-operation-phase-{}", uuid::Uuid::new_v4()));
    let mut journal = Journal::begin(&state, "workspace.stop", "sb/ws").expect("begin journal");
    let id = journal.id().to_string();

    for phase in ["stop_runtime", "cleanup_resources", "verify_cleanup"] {
        journal.phase(phase).expect("record phase");
        let on_disk = load(&state, &id).expect("journal is readable after each phase");
        assert_eq!(on_disk.phase, phase);
    }

    fs::remove_dir_all(state).expect("remove journal fixture");
}

#[test]
fn journal_records_phase_and_terminal_success() {
    let state = std::env::temp_dir().join(format!("enclave-operation-{}", uuid::Uuid::new_v4()));
    let mut journal = Journal::begin(&state, "workspace.stop", "sb/ws").expect("begin journal");
    let id = journal.id().to_string();
    journal.phase("runtime_stopped").expect("record phase");
    let record = journal.succeed().expect("complete journal");
    assert_eq!(record.id, id);
    assert_eq!(record.status, OperationStatus::Succeeded);
    assert_eq!(record.phase, "complete");
    assert_eq!(load(&state, &id).expect("load journal").id, id);
    fs::remove_dir_all(state).expect("remove journal fixture");
}

#[test]
fn journal_retains_failure_details() {
    let state = std::env::temp_dir().join(format!(
        "enclave-operation-failure-{}",
        uuid::Uuid::new_v4()
    ));
    let journal = Journal::begin(&state, "workspace.destroy", "sb/ws").expect("begin journal");
    let record = journal.fail("veth still exists").expect("fail journal");
    assert_eq!(record.status, OperationStatus::Failed);
    assert_eq!(record.error.as_deref(), Some("veth still exists"));
    fs::remove_dir_all(state).expect("remove journal fixture");
}

#[test]
fn a_journal_uses_the_current_operation_id() {
    let state = std::env::temp_dir().join(format!("enclave-operation-id-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();

    // A request that carries an id makes every journal it opens part of that one
    // operation, so the CLI, the logs, and the journal record agree.
    set_current(Some("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0".to_string()));
    let journal = Journal::begin(&state, "workspace.start", "sandbox/workspace").expect("begin");
    assert_eq!(journal.id(), "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0");
    assert_eq!(current().as_deref(), Some(journal.id()));
    let record = journal.succeed().expect("succeed");
    assert_eq!(record.kind, "workspace.start");
    assert_eq!(load(&state, &record.id).expect("load").id, record.id);

    // Without one, each journal gets its own id.
    set_current(None);
    let first = Journal::begin(&state, "workspace.stop", "sandbox/workspace").expect("begin");
    let second = Journal::begin(&state, "workspace.stop", "sandbox/workspace").expect("begin");
    assert_ne!(first.id(), second.id());
    assert!(current().is_none());

    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn only_a_uuid_shaped_id_is_accepted() {
    assert!(is_valid_id("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0"));
    // The id is used as a file name, so a caller cannot supply a path.
    assert!(!is_valid_id("../../etc/passwd"));
    assert!(!is_valid_id("not-an-id"));
    assert!(!is_valid_id(""));
}

/// A journal directory that cannot be created must be reported, not ignored.
///
/// If this returned success the operation would run with no record of it, and
/// nothing downstream would notice: recovery reads the journal to find what was in
/// flight, so an operation that silently has no record looks like one that never
/// happened.
#[test]
fn beginning_a_journal_in_an_unusable_directory_fails() {
    let state = retention_state("unusable");
    fs::create_dir_all(&state).expect("create state dir");
    // A file where the journal directory belongs.
    fs::write(state.join("operations"), b"").expect("create the blocker file");

    let error = match Journal::begin(&state, "workspace.start", "sb/ws") {
        Ok(_) => panic!("a journal that cannot be written must be reported"),
        Err(error) => error,
    };
    assert!(
        format!("{error:#}").contains("operation journal"),
        "the error must name the journal: {error:#}"
    );

    fs::remove_file(state.join("operations")).expect("remove the blocker");
    fs::remove_dir_all(state).expect("remove journal fixture");
}
