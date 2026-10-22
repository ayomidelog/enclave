use crate::workspace::{cancel_file_path, cancel_workspace_command};
use std::path::PathBuf;

struct TempWorkspace {
    root: PathBuf,
}

impl TempWorkspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "enclave-cancel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(root.join("runtime")).expect("runtime dir");
        Self { root }
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn cancelling_a_workspace_with_no_command_succeeds_as_already_finished() {
    // The caller wanted the workspace free; it is. Reporting an error would make a
    // harmless race look like a failure.
    let workspace = TempWorkspace::new();
    let outcome = cancel_workspace_command(&workspace.root).expect("cancel");
    assert!(!outcome.cancelled);
    assert!(outcome.already_finished);
}

#[test]
fn cancelling_signals_the_published_helper_and_reports_it() {
    // A command runs in the helper's pid namespace, so the daemon signals the helper
    // itself: it is a host process, and it holds the workspace's command mutex for
    // exactly as long as the command runs.
    let workspace = TempWorkspace::new();
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn child");
    let helper_pid = child.id() as i32;
    let starttime = crate::workspace::session::process_starttime_ticks(helper_pid as u32)
        .expect("helper start time");
    std::fs::write(
        cancel_file_path(&workspace.root),
        format!("{helper_pid} {starttime}\n"),
    )
    .expect("publish cancel file");

    let outcome = cancel_workspace_command(&workspace.root).expect("cancel");
    assert!(
        outcome.cancelled,
        "the running command should be reported as cancelled"
    );
    assert!(!outcome.already_finished);

    let status = child.wait().expect("wait");
    assert!(!status.success(), "the child should have been killed");
}

#[test]
fn a_cancel_file_naming_an_exited_helper_is_treated_as_finished() {
    let workspace = TempWorkspace::new();
    // A group id that cannot exist: the command finished between publishing and
    // cancelling, which is the race a cancel has to tolerate.
    std::fs::write(cancel_file_path(&workspace.root), "2147483646 1\n").expect("write");
    let outcome = cancel_workspace_command(&workspace.root).expect("cancel");
    assert!(!outcome.cancelled);
    assert!(outcome.already_finished);
}

#[test]
fn a_malformed_cancel_file_is_an_error_not_a_signal() {
    // Never guess at a process group: a bad value could signal something unrelated.
    let workspace = TempWorkspace::new();
    std::fs::write(cancel_file_path(&workspace.root), "not-a-number\n").expect("write");
    assert!(cancel_workspace_command(&workspace.root).is_err());
}

#[test]
fn a_cancel_file_naming_init_is_refused() {
    let workspace = TempWorkspace::new();
    std::fs::write(cancel_file_path(&workspace.root), "1 1\n").expect("write");
    assert!(cancel_workspace_command(&workspace.root).is_err());
}
