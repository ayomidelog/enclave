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
            let code = wait_pid_exit_code(child)?;
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
