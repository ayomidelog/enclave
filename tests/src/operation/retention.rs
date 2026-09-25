//! Trimming the journal, and the records a trim must never remove.
//!
//! An open record is an operation that may still be running, so the trim is bounded
//! by the terminal records around it rather than by age alone.

use super::*;

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
