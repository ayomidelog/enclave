use super::*;

pub(crate) fn run_workspace_command(args: WorkspaceCommandInternalArgs) -> Result<()> {
    if !crate::workspace::session_process_matches(
        args.runtime_pid,
        Some(args.runtime_starttime_ticks),
    ) {
        bail!(
            "workspace runtime pid {} is not alive or no longer matches the expected process",
            args.runtime_pid
        );
    }

    attach_helper_to_workspace_cgroup(&args.cgroup_path)?;

    let namespaces = NamespaceHandles::from_optional_fds(
        args.runtime_pid,
        [
            args.root_fd,
            args.user_ns_fd,
            args.mount_ns_fd,
            args.pid_ns_fd,
            args.net_ns_fd,
            args.uts_ns_fd,
        ],
    )?;
    enter_workspace_namespaces(&namespaces)?;

    let child = unsafe { fork() }.context("failed to fork after namespace entry")?;
    match child {
        ForkResult::Parent { child } => {
            // The detached helper is the group leader. A TERM sent to the group
            // must reach the command and its descendants, but the leader has to
            // survive the graceful phase so the daemon can still address the
            // group for the later SIGKILL. The child is forked before this is
            // changed, so the command keeps the default TERM disposition.
            if args.hold_process_group {
                unsafe {
                    libc::signal(libc::SIGTERM, libc::SIG_IGN);
                }
            }
            let code = wait_pid_exit_code(child)?;
            if args.hold_process_group {
                wait_for_group_descendants();
            }
            std::process::exit(code);
        }
        ForkResult::Child => {
            if let Err(err) = run_workspace_command_child(
                &namespaces.root_dir,
                &args.cwd,
                &args.sandbox_id,
                &args.workspace_id,
                &args.command,
            ) {
                eprintln!("enclave: {err:#}");
                std::process::exit(1);
            }
            unreachable!("workspace command child should exec or exit with an error");
        }
    }
}

/// Wait until the command's process group has no live member except this
/// helper. This is the detached-job supervisor: the helper stays alive as the
/// group leader, so the recorded pid/start-time identity remains valid for the
/// lifetime of work the command started.
fn wait_for_group_descendants() {
    let pgid = unsafe { libc::getpgrp() };
    let self_pid = std::process::id();
    loop {
        if group_members_except_self(pgid, self_pid).is_empty() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// Terminate descendants left behind by a foreground command.
///
/// A foreground command is not allowed to leave background work behind: that is
/// what the explicit `--background` mode is for. The persistent helper leads the
/// command group, so killing the other members reaches descendants that inherited
/// the group without touching the helper itself.
pub(crate) fn terminate_group_descendants() {
    let pgid = unsafe { libc::getpgrp() };
    let self_pid = std::process::id();
    let descendants = group_members_except_self(pgid, self_pid);
    if descendants.is_empty() {
        return;
    }
    for pid in descendants {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    while std::time::Instant::now() < deadline
        && !group_members_except_self(pgid, self_pid).is_empty()
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    for pid in group_members_except_self(pgid, self_pid) {
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
}

fn group_members_except_self(pgid: libc::pid_t, self_pid: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str().map(str::to_string)?;
            let Ok(pid) = name.parse::<u32>() else {
                return None;
            };
            if pid == self_pid {
                return None;
            }
            let Ok(raw) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                return None;
            };
            let rest = raw.rsplit_once(") ").map(|(_, rest)| rest)?;
            let mut fields = rest.split_whitespace();
            let state = fields.next();
            let group = fields
                .nth(1)
                .and_then(|value| value.parse::<libc::pid_t>().ok());
            (group == Some(pgid) && !matches!(state, Some("Z" | "X" | "x"))).then_some(pid)
        })
        .collect()
}

pub(crate) fn run_workspace_command_child(
    root_dir: &File,
    cwd: &str,
    sandbox_id: &str,
    workspace_id: &str,
    command: &[String],
) -> Result<()> {
    if command.is_empty() {
        bail!("workspace command helper requires a command");
    }

    enter_workspace_root(root_dir)?;
    let effective_cwd = crate::workspace::sanitize_workspace_cwd(cwd);
    crate::workspace::session::apply_exec_restrictions()?;

    let mut cmd = Command::new("/usr/bin/env");
    cmd.arg("-i")
        .arg("HOME=/home")
        .arg("TMPDIR=/tmp")
        .arg("TMP=/tmp")
        .arg("TEMP=/tmp")
        .arg("USER=root")
        .arg("LOGNAME=root")
        .arg("TERM=xterm")
        .arg(format!("PATH={}", crate::workspace::DEFAULT_WORKSPACE_PATH))
        .arg(format!("SANDBOX_ID={sandbox_id}"))
        .arg(format!("WORKSPACE_ID={workspace_id}"))
        .arg("/bin/sh")
        .arg("-c")
        .arg(crate::auth::workspace_env_wrapper_script())
        .arg("sh")
        .arg(&effective_cwd);
    for arg in command {
        cmd.arg(arg);
    }

    let err = cmd.exec();
    Err(err).context("failed to execute workspace command")
}
