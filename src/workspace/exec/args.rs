//! The argument lists the internal helpers are given.
//!
//! Each helper re-enters the workspace's namespaces from file descriptors the
//! daemon passes it, so the arguments name the runtime to verify and the six
//! namespace descriptors to enter. Keeping the builders here means the daemon
//! side and the helper side agree on one shape.

/// Arguments for the helper that writes transferred data into a workspace.
pub(crate) fn workspace_file_receive_args(
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    target: &str,
    cgroup_path: Option<&str>,
    fds: [std::os::fd::RawFd; 6],
) -> Vec<String> {
    let mut args = vec![
        "internal".to_string(),
        "workspace-file-receive".to_string(),
        "--runtime-pid".to_string(),
        runtime_pid.to_string(),
        "--runtime-starttime-ticks".to_string(),
        runtime_starttime_ticks.to_string(),
        "--target".to_string(),
        target.to_string(),
    ];
    if let Some(cgroup_path) = cgroup_path {
        args.push("--cgroup-path".to_string());
        args.push(cgroup_path.to_string());
    }
    append_namespace_fd_args(&mut args, fds);
    args
}

#[cfg(test)]
pub(crate) fn runtime_exec_command_args(
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    sandbox_id: &str,
    workspace_id: &str,
    effective_cwd: &str,
    command: &[String],
) -> Vec<String> {
    runtime_exec_command_args_base(
        runtime_pid,
        runtime_starttime_ticks,
        sandbox_id,
        workspace_id,
        effective_cwd,
        command,
        None,
    )
}

pub(crate) fn runtime_exec_command_args_with_fds(
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    sandbox_id: &str,
    workspace_id: &str,
    effective_cwd: &str,
    command: &[String],
    fds: [std::os::fd::RawFd; 6],
) -> Vec<String> {
    runtime_exec_command_args_base(
        runtime_pid,
        runtime_starttime_ticks,
        sandbox_id,
        workspace_id,
        effective_cwd,
        command,
        Some(fds),
    )
}

fn runtime_exec_command_args_base(
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    sandbox_id: &str,
    workspace_id: &str,
    effective_cwd: &str,
    command: &[String],
    fds: Option<[std::os::fd::RawFd; 6]>,
) -> Vec<String> {
    let mut args = vec![
        "internal".to_string(),
        "workspace-command".to_string(),
        "--runtime-pid".to_string(),
        runtime_pid.to_string(),
        "--runtime-starttime-ticks".to_string(),
        runtime_starttime_ticks.to_string(),
        "--cwd".to_string(),
        effective_cwd.to_string(),
        "--sandbox-id".to_string(),
        sandbox_id.to_string(),
        "--workspace-id".to_string(),
        workspace_id.to_string(),
        "--cgroup-path".to_string(),
        super::super::workspace_cgroup_path(sandbox_id, workspace_id)
            .to_string_lossy()
            .into_owned(),
    ];
    if let Some(fds) = fds {
        append_namespace_fd_args(&mut args, fds);
    }
    args.push("--".to_string());
    args.extend(command.iter().cloned());
    args
}

fn append_namespace_fd_args(args: &mut Vec<String>, fds: [std::os::fd::RawFd; 6]) {
    for (name, fd) in [
        ("--root-fd", fds[0]),
        ("--user-ns-fd", fds[1]),
        ("--mount-ns-fd", fds[2]),
        ("--pid-ns-fd", fds[3]),
        ("--net-ns-fd", fds[4]),
        ("--uts-ns-fd", fds[5]),
    ] {
        args.push(name.to_string());
        args.push(fd.to_string());
    }
}
