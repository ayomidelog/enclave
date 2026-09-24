pub(super) mod runtime_init;
mod session;

use std::ffi::CString;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path};
use std::process::Command;

use anyhow::{bail, Context, Result};
use nix::sched::{setns, CloneFlags};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{fork, ForkResult, Pid};
use serde::{Deserialize, Serialize};

use crate::cli::{
    WorkspaceCommandInternalArgs, WorkspaceFileReceiveArgs, WorkspaceSessionPersistentHelperArgs,
};

pub(crate) use session::{
    run_workspace_session_bootstrap, run_workspace_session_launch, run_workspace_session_loop,
};

#[cfg(test)]
use session::{
    open_ready_file_via_old_root, runtime_tmpfs_mount_flags, tmp_directory_is_usable,
    verify_workspace_tmp_mount, workspace_old_root_path, DirectoryIdentity, RUNTIME_TMPFS_DATA,
    WORKSPACE_TMP_DATA,
};

#[derive(Debug, Deserialize)]
struct PersistentCommandRequest {
    auth_token: String,
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    sandbox_id: String,
    workspace_id: String,
    cwd: String,
    command: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PersistentCommandResponse {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

pub(crate) fn run_workspace_session_persistent_helper(
    args: WorkspaceSessionPersistentHelperArgs,
) -> Result<()> {
    if !crate::workspace::session_process_matches(
        args.runtime_pid,
        Some(args.runtime_starttime_ticks),
    ) {
        bail!("workspace runtime is no longer alive or has changed identity");
    }
    let socket_path = Path::new(&args.helper_socket);
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if let Err(error) = fs::remove_file(socket_path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error)
                .with_context(|| format!("failed to remove {}", socket_path.display()));
        }
    }
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to secure {}", socket_path.display()))?;

    let namespaces = NamespaceHandles::from_optional_fds(
        args.runtime_pid,
        [
            Some(args.root_fd),
            Some(args.user_ns_fd),
            Some(args.mount_ns_fd),
            Some(args.pid_ns_fd),
            Some(args.net_ns_fd),
            Some(args.uts_ns_fd),
        ],
    )?;
    enter_workspace_namespaces(&namespaces)?;

    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(stream) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("persistent helper accept failed"),
        };
        if let Err(error) = handle_persistent_command(&args, &namespaces.root_dir, stream) {
            eprintln!("enclave: persistent command rejected: {error:#}");
        }
    }
    Ok(())
}

fn handle_persistent_command(
    args: &WorkspaceSessionPersistentHelperArgs,
    root_dir: &File,
    mut stream: UnixStream,
) -> Result<()> {
    let mut line = String::new();
    BufReader::new(&mut stream)
        .read_line(&mut line)
        .context("failed to read persistent command request")?;
    let request: PersistentCommandRequest =
        serde_json::from_str(&line).context("invalid persistent command request")?;
    if request.auth_token != args.auth_token
        || request.runtime_pid != args.runtime_pid
        || request.runtime_starttime_ticks != args.runtime_starttime_ticks
        || request.sandbox_id != args.sandbox_id
        || request.workspace_id != args.workspace_id
    {
        bail!("persistent command identity validation failed");
    }
    if request.command.is_empty() || request.command.len() > 256 {
        bail!("persistent command has an invalid argument count");
    }
    if !runtime_pidfd_alive(args.runtime_pidfd) {
        bail!("workspace runtime pidfd is no longer alive");
    }
    let cwd = crate::workspace::sanitize_workspace_cwd(&request.cwd);
    let (stdout_reader, stdout_writer) = create_pipe()?;
    let (stderr_reader, stderr_writer) = create_pipe()?;
    let child = unsafe { fork() }.context("failed to fork persistent command")?;
    match child {
        ForkResult::Parent { child } => {
            drop(stdout_writer);
            drop(stderr_writer);
            let (code, stdout, stderr) = drain_command_pipes(stdout_reader, stderr_reader, child)?;
            let response = PersistentCommandResponse {
                exit_code: code,
                stdout: crate::workspace::session::output_to_string(stdout),
                stderr: crate::workspace::session::output_to_string(stderr),
            };
            serde_json::to_writer(&mut stream, &response)
                .context("failed to write persistent command response")?;
            stream
                .write_all(b"\n")
                .context("failed to terminate persistent command response")?;
            Ok(())
        }
        ForkResult::Child => {
            redirect_pipe_to_stdio(stdout_writer, libc::STDOUT_FILENO)?;
            redirect_pipe_to_stdio(stderr_writer, libc::STDERR_FILENO)?;
            if let Err(error) = run_workspace_command_child(
                root_dir,
                &cwd,
                &request.sandbox_id,
                &request.workspace_id,
                &request.command,
            ) {
                eprintln!("enclave: {error:#}");
                std::process::exit(1);
            }
            unreachable!("persistent command child should exec or exit");
        }
    }
}

fn drain_command_pipes(
    stdout_reader: File,
    stderr_reader: File,
    child: Pid,
) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    set_nonblocking(stdout_reader.as_raw_fd())?;
    set_nonblocking(stderr_reader.as_raw_fd())?;
    let mut stdout_reader = Some(stdout_reader);
    let mut stderr_reader = Some(stderr_reader);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut child_status = None;
    while child_status.is_none() || stdout_reader.is_some() || stderr_reader.is_some() {
        let mut descriptors = Vec::with_capacity(2);
        if let Some(reader) = stdout_reader.as_ref() {
            descriptors.push(libc::pollfd {
                fd: reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        if let Some(reader) = stderr_reader.as_ref() {
            descriptors.push(libc::pollfd {
                fd: reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        if !descriptors.is_empty() {
            let result =
                unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, 100) };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error).context("failed to poll persistent command output");
                }
            }
        }
        if let Some(reader) = stdout_reader.as_ref() {
            if descriptors
                .first()
                .is_some_and(|descriptor| descriptor.revents != 0)
                && drain_pipe(reader, &mut stdout)?
            {
                stdout_reader = None;
            }
        }
        let stderr_ready = if stdout_reader.is_some() {
            descriptors
                .get(1)
                .is_some_and(|descriptor| descriptor.revents != 0)
        } else {
            descriptors
                .first()
                .is_some_and(|descriptor| descriptor.revents != 0)
        };
        if let Some(reader) = stderr_reader.as_ref() {
            if stderr_ready && drain_pipe(reader, &mut stderr)? {
                stderr_reader = None;
            }
        }
        if child_status.is_none() {
            match waitpid(child, Some(WaitPidFlag::WNOHANG))
                .context("failed to poll command child")?
            {
                WaitStatus::Exited(_, code) => child_status = Some(code),
                WaitStatus::Signaled(_, signal, _) => child_status = Some(128 + signal as i32),
                WaitStatus::StillAlive => {}
                _ => {}
            }
        }
    }
    Ok((child_status.unwrap_or(1), stdout, stderr))
}

fn drain_pipe(reader: &File, output: &mut Vec<u8>) -> Result<bool> {
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let amount =
            unsafe { libc::read(reader.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        if amount > 0 {
            let amount = amount as usize;
            if output.len() < crate::workspace::session::MAX_HELPER_OUTPUT_BYTES {
                let remaining = crate::workspace::session::MAX_HELPER_OUTPUT_BYTES - output.len();
                output.extend_from_slice(&buffer[..amount.min(remaining)]);
            }
            continue;
        }
        if amount == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(false);
        }
        return Err(error).context("failed to read persistent command output");
    }
}

fn set_nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to inspect pipe flags");
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to set pipe nonblocking");
    }
    Ok(())
}

fn runtime_pidfd_alive(pidfd: i32) -> bool {
    let mut descriptor = libc::pollfd {
        fd: pidfd,
        events: 0,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    result >= 0 && descriptor.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) == 0
}

fn create_pipe() -> Result<(File, File)> {
    let mut fds = [0; 2];
    let result = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to create command pipe");
    }
    Ok((unsafe { File::from_raw_fd(fds[0]) }, unsafe {
        File::from_raw_fd(fds[1])
    }))
}

fn redirect_pipe_to_stdio(writer: File, target_fd: i32) -> Result<()> {
    let result = unsafe { libc::dup2(writer.as_raw_fd(), target_fd) };
    if result < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to redirect command output");
    }
    Ok(())
}

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

    if !args.cgroup_path.is_empty() {
        crate::sandbox::cgroup::add_process_to_cgroup(
            std::path::Path::new(&args.cgroup_path),
            std::process::id(),
        )
        .context("failed to attach workspace command helper to cgroup")?;
    }

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

pub(crate) fn run_workspace_file_receive(args: WorkspaceFileReceiveArgs) -> Result<()> {
    if !crate::workspace::session_process_matches(
        args.runtime_pid,
        Some(args.runtime_starttime_ticks),
    ) {
        bail!(
            "workspace runtime pid {} is not alive or no longer matches the expected process",
            args.runtime_pid
        );
    }
    validate_workspace_file_target(&args.target)?;
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
            if let Err(error) = receive_workspace_file(&namespaces.root_dir, &args.target) {
                eprintln!("enclave: {error:#}");
                std::process::exit(1);
            }
            std::process::exit(0);
        }
    }
}

fn receive_workspace_file(root_dir: &File, target: &str) -> Result<()> {
    enter_workspace_root(root_dir)?;
    crate::workspace::session::apply_exec_restrictions()?;
    let mut destination = open_workspace_transfer_target(target)?;
    std::io::copy(&mut std::io::stdin(), &mut destination)
        .context("failed to receive workspace file data")?;
    destination
        .sync_all()
        .context("failed to sync workspace file")?;
    Ok(())
}

fn open_workspace_transfer_target(target: &str) -> Result<File> {
    let path = Path::new(target);
    let mut parent = File::open("/home").context("failed to open workspace home")?;
    let components = path.components().collect::<Vec<_>>();
    let Some(file_name) = components.last().copied() else {
        bail!("workspace transfer target must name a file: {target}");
    };
    for component in components
        .iter()
        .skip(2)
        .take(components.len().saturating_sub(3))
    {
        let Component::Normal(name) = component else {
            bail!("workspace transfer target contains unsafe path components: {target}");
        };
        let name = CString::new(name.as_bytes())
            .context("workspace transfer directory contains an interior NUL byte")?;
        let descriptor = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("failed to open workspace transfer directory {name:?}"));
        }
        parent = unsafe { File::from_raw_fd(descriptor) };
    }
    let Component::Normal(file_name) = file_name else {
        bail!("workspace transfer target must name a regular file: {target}");
    };
    let file_name = CString::new(file_name.as_bytes())
        .context("workspace transfer file contains an interior NUL byte")?;
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            file_name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to create workspace transfer target {target}"));
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn validate_workspace_file_target(target: &str) -> Result<()> {
    let path = Path::new(target);
    if !path.is_absolute() || !path.starts_with("/home") {
        bail!("workspace transfer target must be under /home: {target}");
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::CurDir | Component::Prefix(_)
        )
    }) {
        bail!("workspace transfer target contains unsafe path components: {target}");
    }
    Ok(())
}

struct NamespaceHandles {
    root_dir: File,
    user_ns: File,
    mount_ns: File,
    pid_ns: File,
    net_ns: File,
    uts_ns: File,
}

impl NamespaceHandles {
    fn from_optional_fds(runtime_pid: u32, fds: [Option<i32>; 6]) -> Result<Self> {
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

    fn open(runtime_pid: u32) -> Result<Self> {
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

fn enter_workspace_namespaces(namespaces: &NamespaceHandles) -> Result<()> {
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

fn run_workspace_command_child(
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

fn enter_workspace_root(root_dir: &File) -> Result<()> {
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

fn wait_pid_exit_code(pid: Pid) -> Result<i32> {
    loop {
        match waitpid(pid, None).context("waitpid failed")? {
            WaitStatus::Exited(_, code) => return Ok(code),
            WaitStatus::Signaled(_, signal, _) => return Ok(128 + signal as i32),
            WaitStatus::StillAlive => continue,
            _ => continue,
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/src/commands/internal.rs"]
mod tests;
