use crate::workspace::{cancel_workspace_command, command_marker_dir};
use std::os::unix::process::CommandExt;
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

fn publish_marker(workspace: &TempWorkspace, helper_pid: i32, starttime: u64) -> PathBuf {
    let directory = command_marker_dir(&workspace.root);
    std::fs::create_dir_all(&directory).expect("marker directory");
    let path = directory.join(format!("test-{helper_pid}.pgid"));
    std::fs::write(&path, format!("{helper_pid} {starttime}\n")).expect("publish marker");
    path
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
    // itself: it is a host process and the leader of the command's process group.
    let workspace = TempWorkspace::new();
    let mut command = std::process::Command::new("/bin/sleep");
    command.arg("30");
    // The persistent helper is a process-group leader, so its published pid is
    // also the group id that contains the command.
    // SAFETY: `setpgid` is async-signal-safe and runs before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let mut child = command.spawn().expect("spawn child");
    let helper_pid = child.id() as i32;
    let starttime = crate::workspace::session::process_starttime_ticks(helper_pid as u32)
        .expect("helper start time");
    publish_marker(&workspace, helper_pid, starttime);

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
fn cancelling_reaches_the_command_descendant_too() {
    let workspace = TempWorkspace::new();
    let child_pid_path = workspace.root.join("runtime/descendant.pid");
    let script = format!("sleep 30 & echo $! > '{}'; wait", child_pid_path.display());
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", &script]);
    // SAFETY: `setpgid` is async-signal-safe and runs before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let mut helper = command.spawn().expect("spawn helper-like parent");
    let helper_pid = helper.id();
    let starttime =
        crate::workspace::session::process_starttime_ticks(helper_pid).expect("helper start time");
    publish_marker(&workspace, helper_pid as i32, starttime);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !child_pid_path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "descendant did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let child_pid: u32 = std::fs::read_to_string(&child_pid_path)
        .expect("read descendant pid")
        .trim()
        .parse()
        .expect("parse descendant pid");

    cancel_workspace_command(&workspace.root).expect("cancel command group");
    let _ = helper.wait();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while crate::workspace::session_process_matches(child_pid, None) {
        assert!(
            std::time::Instant::now() < deadline,
            "the command descendant survived cancellation"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn a_cancel_file_naming_an_exited_helper_is_treated_as_finished() {
    let workspace = TempWorkspace::new();
    // A group id that cannot exist: the command finished between publishing and
    // cancelling, which is the race a cancel has to tolerate.
    publish_marker(&workspace, 2_147_483_646, 1);
    let outcome = cancel_workspace_command(&workspace.root).expect("cancel");
    assert!(!outcome.cancelled);
    assert!(outcome.already_finished);
}

#[test]
fn a_malformed_cancel_file_is_an_error_not_a_signal() {
    // Never guess at a process group: a bad value could signal something unrelated.
    let workspace = TempWorkspace::new();
    let path = publish_marker(&workspace, 2_147_483_646, 1);
    std::fs::write(path, "not-a-number\n").expect("write");
    assert!(cancel_workspace_command(&workspace.root).is_err());
}

#[test]
fn a_cancel_file_naming_init_is_refused() {
    let workspace = TempWorkspace::new();
    publish_marker(&workspace, 1, 1);
    assert!(cancel_workspace_command(&workspace.root).is_err());
}

fn spawn_group_leader() -> std::process::Child {
    let mut command = std::process::Command::new("/bin/sleep");
    command.arg("30");
    // SAFETY: `setpgid` is async-signal-safe and runs before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    command.spawn().expect("spawn process-group leader")
}

#[test]
fn cancelling_stops_every_published_command_group() {
    let workspace = TempWorkspace::new();
    let mut first = spawn_group_leader();
    let mut second = spawn_group_leader();
    for child in [&first, &second] {
        let pid = child.id();
        let starttime =
            crate::workspace::session::process_starttime_ticks(pid).expect("helper start time");
        publish_marker(&workspace, pid as i32, starttime);
    }

    let outcome = cancel_workspace_command(&workspace.root).expect("cancel command groups");
    assert!(outcome.cancelled);
    assert!(!outcome.already_finished);
    assert!(!first.wait().expect("wait for first group").success());
    assert!(!second.wait().expect("wait for second group").success());
}

#[test]
fn one_malformed_marker_prevents_signalling_a_valid_group() {
    let workspace = TempWorkspace::new();
    let mut child = spawn_group_leader();
    let pid = child.id();
    let starttime =
        crate::workspace::session::process_starttime_ticks(pid).expect("helper start time");
    publish_marker(&workspace, pid as i32, starttime);
    std::fs::write(
        command_marker_dir(&workspace.root).join("malformed.pgid"),
        "not-a-number\n",
    )
    .expect("write malformed marker");

    assert!(cancel_workspace_command(&workspace.root).is_err());
    assert!(
        child.try_wait().expect("inspect child").is_none(),
        "a malformed marker must prevent every signal in the same cancel request"
    );
    let _ = child.kill();
    let _ = child.wait();
}
