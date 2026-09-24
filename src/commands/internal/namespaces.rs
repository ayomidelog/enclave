use super::*;

/// Attach this helper process to a workspace cgroup before it forks the process
/// that will run inside the workspace.
///
/// Children inherit the cgroup, which is what makes the workspace's declared
/// CPU, memory, and process limits apply to work started through the helper. An
/// empty path means the host has no workspace cgroup to attach to.
pub(crate) fn attach_helper_to_workspace_cgroup(cgroup_path: &str) -> Result<()> {
    if cgroup_path.is_empty() {
        return Ok(());
    }
    crate::sandbox::cgroup::add_process_to_cgroup(
        std::path::Path::new(cgroup_path),
        std::process::id(),
    )
    .context("failed to attach workspace helper to cgroup")
}

pub(crate) struct NamespaceHandles {
    pub(crate) root_dir: File,
    user_ns: File,
    mount_ns: File,
    pid_ns: File,
    net_ns: File,
    uts_ns: File,
}

impl NamespaceHandles {
    pub(crate) fn from_optional_fds(runtime_pid: u32, fds: [Option<i32>; 6]) -> Result<Self> {
        if let [Some(root), Some(user), Some(mount), Some(pid), Some(net), Some(uts)] = fds {
            return Ok(Self {
                root_dir: unsafe { File::from_raw_fd(root) },
                user_ns: unsafe { File::from_raw_fd(user) },
                mount_ns: unsafe { File::from_raw_fd(mount) },
                pid_ns: unsafe { File::from_raw_fd(pid) },
                net_ns: unsafe { File::from_raw_fd(net) },
                uts_ns: unsafe { File::from_raw_fd(uts) },
            });
        }
        Self::open(runtime_pid)
    }

    pub(crate) fn open(runtime_pid: u32) -> Result<Self> {
        let root_dir = File::open(format!("/proc/{runtime_pid}/root"))
            .with_context(|| format!("failed to open /proc/{runtime_pid}/root"))?;
        let user_ns = File::open(format!("/proc/{runtime_pid}/ns/user"))
            .with_context(|| format!("failed to open /proc/{runtime_pid}/ns/user"))?;
        let mount_ns = File::open(format!("/proc/{runtime_pid}/ns/mnt"))
            .with_context(|| format!("failed to open /proc/{runtime_pid}/ns/mnt"))?;
        let pid_ns = File::open(format!("/proc/{runtime_pid}/ns/pid"))
            .with_context(|| format!("failed to open /proc/{runtime_pid}/ns/pid"))?;
        let net_ns = File::open(format!("/proc/{runtime_pid}/ns/net"))
            .with_context(|| format!("failed to open /proc/{runtime_pid}/ns/net"))?;
        let uts_ns = File::open(format!("/proc/{runtime_pid}/ns/uts"))
            .with_context(|| format!("failed to open /proc/{runtime_pid}/ns/uts"))?;

        Ok(Self {
            root_dir,
            user_ns,
            mount_ns,
            pid_ns,
            net_ns,
            uts_ns,
        })
    }
}

pub(crate) fn enter_workspace_namespaces(namespaces: &NamespaceHandles) -> Result<()> {
    match setns(&namespaces.user_ns, CloneFlags::CLONE_NEWUSER) {
        Ok(()) => {}
        Err(nix::errno::Errno::EINVAL) => {
            tracing::debug!("workspace runtime already shares the caller's user namespace");
        }
        Err(err) => return Err(err).context("setns user namespace failed"),
    }
    setns(&namespaces.mount_ns, CloneFlags::CLONE_NEWNS).context("setns mount namespace failed")?;
    setns(&namespaces.pid_ns, CloneFlags::CLONE_NEWPID).context("setns pid namespace failed")?;
    setns(&namespaces.net_ns, CloneFlags::CLONE_NEWNET)
        .context("setns network namespace failed")?;
    setns(&namespaces.uts_ns, CloneFlags::CLONE_NEWUTS).context("setns uts namespace failed")?;
    Ok(())
}

pub(crate) fn wait_pid_exit_code(pid: Pid) -> Result<i32> {
    loop {
        match waitpid(pid, None).context("waitpid failed")? {
            WaitStatus::Exited(_, code) => return Ok(code),
            WaitStatus::Signaled(_, signal, _) => return Ok(128 + signal as i32),
            WaitStatus::StillAlive => continue,
            _ => continue,
        }
    }
}

pub(crate) fn enter_workspace_root(root_dir: &File) -> Result<()> {
    let dot = CString::new(".").expect("literal dot contains no nul");
    let rc = unsafe { libc::fchdir(root_dir.as_raw_fd()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("fchdir to workspace root failed");
    }
    let rc = unsafe { libc::chroot(dot.as_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("chroot into workspace root failed");
    }
    std::env::set_current_dir("/").context("failed to change directory to / after chroot")?;
    Ok(())
}
