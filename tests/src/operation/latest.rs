//! Which record a status report names as the last operation.

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
