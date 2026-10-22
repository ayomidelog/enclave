use super::*;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

fn process_state(pid: u32) -> Option<char> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    raw.rsplit_once(") ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .and_then(|state| state.chars().next())
}

/// A process that exited but has not been reaped still appears in `/proc`.
/// It must not make its process group look alive forever.
#[test]
fn a_zombie_is_not_a_live_process_group_member() {
    let mut command = Command::new("sh");
    command
        .args(["-c", "exec sleep 30"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
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
    let mut child = command.spawn().expect("spawn a process-group leader");
    let pgid = i32::try_from(child.id()).expect("pid fits an i32");
    let starttime = session::process_starttime_ticks(child.id()).expect("child start time");
    assert!(process_group_has_members(pgid, Some(starttime)));

    child.kill().expect("kill the group leader");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while process_state(child.id()) != Some('Z') {
        assert!(
            std::time::Instant::now() < deadline,
            "the child never became a zombie"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!process_group_has_members(pgid, Some(starttime)));
    let _ = child.wait();
}
