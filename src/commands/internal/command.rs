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
        if !has_other_group_member(pgid, self_pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

fn has_other_group_member(pgid: libc::pid_t, self_pid: u32) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            return false;
        };
        let Ok(pid) = name.parse::<u32>() else {
            return false;
        };
        if pid == self_pid {
            return false;
        }
        let Ok(raw) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        let Some(rest) = raw.rsplit_once(") ").map(|(_, rest)| rest) else {
            return false;
        };
        let mut fields = rest.split_whitespace();
        let state = fields.next();
        let group = fields
            .nth(1)
            .and_then(|value| value.parse::<libc::pid_t>().ok());
        group == Some(pgid) && !matches!(state, Some("Z" | "X" | "x"))
    })
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
