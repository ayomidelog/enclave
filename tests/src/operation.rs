use super::*;

/// The latest operation is the one an operator wants first, and it has to be
/// found by update time because the file name is a UUID with no ordering.
#[test]
fn the_latest_operation_is_the_most_recently_updated_one() {
    let state =
        std::env::temp_dir().join(format!("enclave-operation-latest-{}", uuid::Uuid::new_v4()));
    assert!(latest(&state).expect("no journal directory yet").is_none());

    let first = Journal::begin(&state, "workspace.start", "sb/one").expect("begin first");
    let first_id = first.id().to_string();
    let first = first.succeed().expect("finish first");

    let second = Journal::begin(&state, "workspace.stop", "sb/two").expect("begin second");
    let second_id = second.id().to_string();
    let second = second.fail("veth still exists").expect("fail second");

    let latest = latest(&state).expect("read latest").expect("a record");
    assert_eq!(latest.id, second_id);
    assert_eq!(latest.status, OperationStatus::Failed);
    assert_eq!(latest.error.as_deref(), Some("veth still exists"));
    assert_ne!(latest.id, first_id);
    assert!(first.updated_at <= second.updated_at);

    fs::remove_dir_all(state).expect("remove journal fixture");
}

/// A malformed record is doctor's finding to report, not a reason for the lookup
/// to fail and hide the good records beside it.
#[test]
fn the_latest_operation_ignores_malformed_records() {
    let state = std::env::temp_dir().join(format!(
        "enclave-operation-malformed-{}",
        uuid::Uuid::new_v4()
    ));
    let journal = Journal::begin(&state, "sandbox.stop", "sb").expect("begin journal");
    let id = journal.id().to_string();
    journal.succeed().expect("finish journal");

    fs::write(state.join("operations").join("broken.json"), b"{ not json")
        .expect("write malformed record");

    let latest = latest(&state).expect("read latest").expect("a record");
    assert_eq!(latest.id, id);

    fs::remove_dir_all(state).expect("remove journal fixture");
}

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

/// A record left open by a daemon that died is closed by the next daemon.
///
/// Without this, the record stays open forever and every later doctor run reports
/// it as unfinished, which is what makes the journal grow into a list of
/// operations that look permanently in flight.
#[test]
fn closing_unfinished_records_closes_every_open_one() {
    let state =
        std::env::temp_dir().join(format!("enclave-operation-close-{}", uuid::Uuid::new_v4()));

    let open = Journal::begin(&state, "workspace.start", "sb/open").expect("begin open journal");
    let open_id = open.id().to_string();
    let finished = Journal::begin(&state, "workspace.stop", "sb/done").expect("begin finished");
    let finished_id = finished.id().to_string();
    finished.succeed().expect("finish journal");

    let closed = close_unfinished_records(&state, "interrupted by a daemon restart")
        .expect("close unfinished records");
    assert_eq!(closed, vec![open_id.clone()]);

    let record = load(&state, &open_id).expect("read closed record");
    assert_eq!(record.status, OperationStatus::Failed);
    assert_eq!(
        record.error.as_deref(),
        Some("interrupted by a daemon restart")
    );

    // A record that already reached a terminal state keeps its own outcome.
    let record = load(&state, &finished_id).expect("read finished record");
    assert_eq!(record.status, OperationStatus::Succeeded);

    // A second pass finds nothing to do.
    let closed = close_unfinished_records(&state, "interrupted by a daemon restart")
        .expect("close unfinished records again");
    assert!(closed.is_empty());

    fs::remove_dir_all(state).expect("remove journal fixture");
}

/// Closing is not blocked by a record that cannot be read.
#[test]
fn closing_unfinished_records_skips_malformed_files() {
    let state = std::env::temp_dir().join(format!(
        "enclave-operation-close-malformed-{}",
        uuid::Uuid::new_v4()
    ));
    let open = Journal::begin(&state, "workspace.start", "sb/open").expect("begin open journal");
    let open_id = open.id().to_string();
    fs::write(state.join("operations").join("broken.json"), b"{ not json")
        .expect("write malformed record");

    let closed = close_unfinished_records(&state, "interrupted").expect("close unfinished records");
    assert_eq!(closed, vec![open_id]);

    fs::remove_dir_all(state).expect("remove journal fixture");
}

/// A state directory with no journal is not an error.
#[test]
fn closing_unfinished_records_on_an_empty_state_dir_is_a_no_op() {
    let state = std::env::temp_dir().join(format!(
        "enclave-operation-close-empty-{}",
        uuid::Uuid::new_v4()
    ));
    let closed = close_unfinished_records(&state, "interrupted").expect("close unfinished records");
    assert!(closed.is_empty());
}

/// Write one journal record directly, so a retention test can build a journal far
/// larger than the limit without paying a durable write for each record.
fn write_record(root: &std::path::Path, id: &str, status: OperationStatus, updated_at: &str) {
    let record = OperationRecord {
        id: id.to_string(),
        kind: "workspace.start".to_string(),
        target: "sb/ws".to_string(),
        status,
        phase: "complete".to_string(),
        started_at: updated_at.to_string(),
        updated_at: updated_at.to_string(),
        error: None,
    };
    fs::create_dir_all(root).expect("create journal directory");
    fs::write(
        root.join(format!("{id}.json")),
        serde_json::to_vec(&record).expect("encode record"),
    )
    .expect("write record");
}

fn retention_state(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "enclave-operation-retention-{}-{label}",
        uuid::Uuid::new_v4()
    ))
}

/// A journal inside the limit is left exactly as it is.
///
/// This is the usual case, and it is the one that has to stay cheap: the trim runs
/// on every daemon start.
#[test]
fn a_journal_inside_the_retention_limit_is_untouched() {
    let state = retention_state("inside");
    let root = state.join("operations");
    for index in 0..5 {
        write_record(
            &root,
            &format!("record-{index}"),
            OperationStatus::Succeeded,
            &format!("2026-01-0{}T00:00:00.000Z", index + 1),
        );
    }

    assert_eq!(prune_terminal_records(&state).expect("prune"), 0);
    assert_eq!(fs::read_dir(&root).expect("read journal").count(), 5);

    fs::remove_dir_all(state).expect("remove journal fixture");
}

/// Beyond the limit the oldest terminal records go and the newest stay.
///
/// The newest are the ones an operator is most likely to ask about, and the
/// journal exists to answer that question rather than to keep everything forever.
#[test]
fn terminal_records_beyond_the_retention_limit_are_trimmed_oldest_first() {
    let state = retention_state("trim");
    let root = state.join("operations");
    let total = JOURNAL_TERMINAL_LIMIT + 7;
    for index in 0..total {
        write_record(
            &root,
            &format!("record-{index:05}"),
            OperationStatus::Succeeded,
            // Ascending timestamps, so record 0 is the oldest.
            &format!("2026-01-01T00:{:02}:{:02}.000Z", index / 60, index % 60),
        );
    }

    assert_eq!(
        prune_terminal_records(&state).expect("prune"),
        7,
        "the seven records past the limit must be the ones removed"
    );
    assert_eq!(
        fs::read_dir(&root).expect("read journal").count(),
        JOURNAL_TERMINAL_LIMIT
    );
    for index in 0..7 {
        assert!(
            !root.join(format!("record-{index:05}.json")).exists(),
            "record {index} is among the oldest and should have been trimmed"
        );
    }
    assert!(
        root.join(format!("record-{:05}.json", total - 1)).exists(),
        "the newest record must survive the trim"
    );

    fs::remove_dir_all(state).expect("remove journal fixture");
}

/// A record that is not terminal is never trimmed, however old it is.
///
/// An open record is the input recovery reads, so removing one would delete the
/// only description of work a previous daemon left unfinished.
#[test]
fn an_unfinished_record_survives_the_retention_trim() {
    let state = retention_state("unfinished");
    let root = state.join("operations");
    let total = JOURNAL_TERMINAL_LIMIT + 3;
    // The oldest record is the one that is still running.
    write_record(
        &root,
        "record-unfinished",
        OperationStatus::Running,
        "2025-01-01T00:00:00.000Z",
    );
    for index in 0..total {
        write_record(
            &root,
            &format!("record-{index:05}"),
            OperationStatus::Succeeded,
            &format!("2026-01-01T00:{:02}:{:02}.000Z", index / 60, index % 60),
        );
    }

    assert_eq!(
        prune_terminal_records(&state).expect("prune"),
        3,
        "only terminal records count against the limit"
    );
    assert!(
        root.join("record-unfinished.json").exists(),
        "an unfinished record is recovery input and must never be trimmed"
    );
    assert_eq!(
        fs::read_dir(&root).expect("read journal").count(),
        JOURNAL_TERMINAL_LIMIT + 1
    );

    fs::remove_dir_all(state).expect("remove journal fixture");
}
