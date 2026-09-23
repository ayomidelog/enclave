use super::*;

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
