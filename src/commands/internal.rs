use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use nix::mount::{mount, MsFlags};
use nix::sched::{setns, CloneFlags};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{fork, ForkResult, Pid};
use serde::{Deserialize, Serialize};

use crate::cli::{
    WorkspaceCommandInternalArgs, WorkspaceFileReceiveArgs, WorkspaceSessionBootstrapArgs,
    WorkspaceSessionLaunchArgs, WorkspaceSessionLoopArgs, WorkspaceSessionPersistentHelperArgs,
};

const PIVOTED_OLD_ROOT: &str = "/.old_root";
const RUNTIME_TMPFS_DATA: &str = "mode=700";

pub(crate) fn run_workspace_session_launch(args: WorkspaceSessionLaunchArgs) -> Result<()> {
    let (mut parent_sync, mut child_sync) =
        UnixStream::pair().context("failed to create workspace launch sync pipe")?;
    let child = unsafe { fork() }.context("failed to fork workspace session launcher")?;
    match child {
        ForkResult::Parent { child } => {
            drop(child_sync);
            wait_for_child_unshare(&mut parent_sync)?;
            if args.enable_userns {
                apply_workspace_id_maps(child.as_raw() as u32, &args)?;
            }
            parent_sync
                .write_all(&[1])
                .context("failed to signal workspace launcher child after id map setup")?;
            Ok(())
        }
        ForkResult::Child => {
            drop(parent_sync);
            unshare_workspace_namespaces(args.enable_userns)?;
            child_sync
                .write_all(&[1])
                .context("failed to notify parent after namespace unshare")?;
            let mut ack = [0u8; 1];
            child_sync
                .read_exact(&mut ack)
                .context("failed to wait for parent id map setup")?;
            if args.enable_userns {
                finalize_workspace_identity()?;
            }

            let grandchild =
                unsafe { fork() }.context("failed to fork into workspace pid namespace")?;
            match grandchild {
                ForkResult::Parent { .. } => {
                    std::process::exit(0);
                }
                ForkResult::Child => exec_workspace_session_script(&args),
            }
        }
    }
}

pub(crate) fn run_workspace_session_bootstrap(args: WorkspaceSessionBootstrapArgs) -> Result<()> {
    let rootfs = validate_workspace_rootfs(Path::new(&args.rootfs))?;
    let (new_root, host_old_root) = if args.root_overlay_merged.is_empty() {
        let host_old_root = rootfs.join(".old_root");
        fs::create_dir_all(&host_old_root)
            .with_context(|| format!("failed to create {}", host_old_root.display()))?;
        bind_mount_self(&rootfs)?;
        (rootfs, host_old_root)
    } else {
        let upper = validate_workspace_overlay_path(&args.root_overlay_upper, "upper")?;
        let work = validate_workspace_overlay_path(&args.root_overlay_work, "work")?;
        let merged = validate_workspace_overlay_path(&args.root_overlay_merged, "merged")?;
        mount_workspace_root_overlay(&rootfs, &upper, &work, &merged)?;
        let host_old_root = merged.join(".old_root");
        fs::create_dir_all(&host_old_root)
            .with_context(|| format!("failed to create {}", host_old_root.display()))?;
        (merged, host_old_root)
    };

    pivot_into_rootfs(&new_root, &host_old_root)?;
    std::env::set_current_dir("/").context("failed to chdir to / after pivot_root")?;
    mount_workspace_source(
        Path::new(PIVOTED_OLD_ROOT),
        Path::new(&args.workspace_fs),
        Path::new(&args.mount_target),
        &args.workspace_idmap_option,
    )?;
    mount_post_pivot_filesystems(
        Path::new(PIVOTED_OLD_ROOT),
        Path::new(&args.workspace_fs),
        &args.workspace_idmap_option,
        args.disk_backed_tmp,
    )?;
    run_workspace_session_loop_inner(Path::new(PIVOTED_OLD_ROOT), Path::new(&args.ready_file))
}

fn validate_workspace_overlay_path(raw: &str, label: &str) -> Result<PathBuf> {
    let path = PathBuf::from(raw);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        bail!(
            "workspace root overlay {label} path is unsafe: {}",
            path.display()
        );
    }
    Ok(path)
}

fn mount_workspace_root_overlay(
    lower: &Path,
    upper: &Path,
    work: &Path,
    merged: &Path,
) -> Result<()> {
    for path in [upper, work, merged] {
        fs::create_dir_all(path)
            .with_context(|| format!("failed to create root overlay path {}", path.display()))?;
    }
    let options = format!(
        "lowerdir={},upperdir={},workdir={}",
        overlay_mount_path(lower),
        overlay_mount_path(upper),
        overlay_mount_path(work)
    );
    mount(
        Option::<&str>::None,
        merged,
        Some("overlay"),
        MsFlags::empty(),
        Some(options.as_str()),
    )
    .with_context(|| {
        format!(
            "failed to mount workspace root overlay at {}",
            merged.display()
        )
    })?;
    crate::perf::record_mount();
    Ok(())
}

fn overlay_mount_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\134")
        .replace(',', "\\054")
}

pub(crate) fn run_workspace_session_loop(args: WorkspaceSessionLoopArgs) -> Result<()> {
    run_workspace_session_loop_inner(Path::new(&args.old_root), Path::new(&args.ready_file))
}

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

fn open_ready_file_via_old_root(old_root: &Path, ready_file: &Path) -> Result<File> {
    if !ready_file.is_absolute() {
        bail!("ready file path must be absolute: {}", ready_file.display());
    }
    let relative = ready_file
        .strip_prefix("/")
        .expect("absolute path strips leading slash");
    if relative
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        bail!(
            "ready file path must not contain traversal components: {}",
            ready_file.display()
        );
    }
    let path = old_root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))
}

fn mount_workspace_source(
    old_root: &Path,
    workspace_fs: &Path,
    mount_target: &Path,
    workspace_idmap_option: &str,
) -> Result<()> {
    if !workspace_fs.is_absolute() {
        bail!(
            "workspace source path must be absolute: {}",
            workspace_fs.display()
        );
    }
    if !mount_target.is_absolute() {
        bail!(
            "workspace mount target must be absolute: {}",
            mount_target.display()
        );
    }

    let source_relative = workspace_fs
        .strip_prefix("/")
        .expect("absolute source strips leading slash");
    if source_relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::CurDir | Component::Prefix(_)
        )
    }) {
        bail!(
            "workspace source path must not contain traversal components: {}",
            workspace_fs.display()
        );
    }
    let source = old_root.join(source_relative);
    if !source.exists() {
        bail!(
            "workspace source path does not exist inside old root: {}",
            source.display()
        );
    }

    fs::create_dir_all(mount_target)
        .with_context(|| format!("failed to create {}", mount_target.display()))?;
    if is_mountpoint(mount_target)? {
        return Ok(());
    }

    if workspace_idmap_option.is_empty() {
        mount(
            Some(source.as_path()),
            mount_target,
            Option::<&str>::None,
            MsFlags::MS_BIND,
            Option::<&str>::None,
        )
        .with_context(|| {
            format!(
                "failed to bind workspace source {} to {}",
                source.display(),
                mount_target.display()
            )
        })?;
        return Ok(());
    }

    for candidate in ["/bin/mount", "/usr/bin/mount"] {
        let mount_binary = Path::new(candidate);
        if !mount_binary.exists() {
            continue;
        }
        let status = Command::new(mount_binary)
            .arg("--bind")
            .arg("-o")
            .arg(format!("X-mount.idmap={workspace_idmap_option}"))
            .arg(&source)
            .arg(mount_target)
            .status()
            .with_context(|| format!("failed to execute {}", mount_binary.display()))?;
        if status.success() {
            return Ok(());
        }
        bail!(
            "idmapped workspace bind mount failed via {} with status {}",
            mount_binary.display(),
            status
        );
    }

    bail!("idmapped workspace bind mount requires /bin/mount or /usr/bin/mount inside the rootfs");
}

fn run_workspace_session_loop_inner(old_root: &Path, ready_file: &Path) -> Result<()> {
    let ready_handle = open_ready_file_via_old_root(old_root, ready_file)?;
    crate::workspace::session::mask_runtime_paths()?;
    crate::workspace::session::tighten_namespace_mounts()?;
    crate::workspace::session::detach_old_root(old_root)?;
    crate::workspace::session::apply_session_restrictions()?;
    signal_ready(ready_handle)?;

    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

fn signal_ready(mut ready_handle: File) -> Result<()> {
    ready_handle
        .write_all(b"ready\n")
        .context("failed to write ready marker")?;
    ready_handle
        .flush()
        .context("failed to flush ready marker")?;
    Ok(())
}

fn unshare_workspace_namespaces(enable_userns: bool) -> Result<()> {
    let mut flags =
        libc::CLONE_NEWNS | libc::CLONE_NEWPID | libc::CLONE_NEWNET | libc::CLONE_NEWUTS;
    if enable_userns {
        flags |= libc::CLONE_NEWUSER;
    }
    let rc = unsafe { libc::unshare(flags) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to unshare workspace namespaces");
    }
    Ok(())
}

fn wait_for_child_unshare(sync: &mut UnixStream) -> Result<()> {
    let mut ready = [0u8; 1];
    sync.read_exact(&mut ready)
        .context("workspace launcher child exited before namespace setup completed")
}

fn apply_workspace_id_maps(pid: u32, args: &WorkspaceSessionLaunchArgs) -> Result<()> {
    if args.deny_setgroups {
        write_proc_file(&format!("/proc/{pid}/setgroups"), "deny\n")
            .context("failed to disable setgroups before gid_map write")?;
    }
    write_id_map(
        &format!("/proc/{pid}/uid_map"),
        args.uid_inner,
        args.uid_outer,
        args.uid_count,
    )
    .context("failed to write uid_map")?;
    write_id_map(
        &format!("/proc/{pid}/gid_map"),
        args.gid_inner,
        args.gid_outer,
        args.gid_count,
    )
    .context("failed to write gid_map")?;
    Ok(())
}

fn finalize_workspace_identity() -> Result<()> {
    let rc = unsafe { libc::setresgid(0, 0, 0) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to setresgid(0,0,0)");
    }
    let rc = unsafe { libc::setresuid(0, 0, 0) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to setresuid(0,0,0)");
    }
    Ok(())
}

fn write_id_map(path: &str, inner: u32, outer: u32, count: u32) -> Result<()> {
    write_proc_file(path, &format!("{inner} {outer} {count}\n"))
}

fn write_proc_file(path: &str, contents: &str) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("failed to write {}", path))
}

fn exec_workspace_session_script(args: &WorkspaceSessionLaunchArgs) -> Result<()> {
    let err = Command::new("/bin/sh")
        .arg("-ceu")
        .arg(crate::workspace::session::WORKSPACE_SESSION_SCRIPT)
        .arg("enclave-workspace-session")
        .arg(&args.rootfs)
        .arg(&args.workspace_fs)
        .arg(&args.mount_target)
        .arg(&args.mount_ref)
        .arg(&args.pid_ref)
        .arg(&args.pid_file)
        .arg(&args.ready_file)
        .arg(&args.cpu_limit)
        .arg(&args.memory_limit_kb)
        .arg(&args.proc_limit)
        .arg(&args.nofile_limit)
        .arg(&args.workspace_hostname)
        .arg(&args.session_helper)
        .arg(&args.apparmor_profile)
        .arg(&args.selinux_label)
        .arg(&args.workspace_idmap_option)
        .arg(if args.disk_backed_tmp { "true" } else { "" })
        .arg(&args.root_overlay_upper)
        .arg(&args.root_overlay_work)
        .arg(&args.root_overlay_merged)
        .exec();
    Err(err).context("failed to exec workspace session bootstrap script")
}

fn validate_workspace_rootfs(rootfs: &Path) -> Result<PathBuf> {
    if !rootfs.is_absolute() {
        bail!(
            "workspace rootfs path must be absolute: {}",
            rootfs.display()
        );
    }
    if rootfs == Path::new("/") {
        bail!("workspace rootfs path must not be /");
    }
    if rootfs
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        bail!(
            "workspace rootfs path must not contain traversal components: {}",
            rootfs.display()
        );
    }
    Ok(rootfs.to_path_buf())
}

fn bind_mount_self(path: &Path) -> Result<()> {
    mount(
        Some(path),
        path,
        Option::<&str>::None,
        MsFlags::MS_BIND,
        Option::<&str>::None,
    )
    .with_context(|| format!("failed to bind-mount {}", path.display()))?;
    crate::perf::record_mount();
    Ok(())
}

fn pivot_into_rootfs(rootfs: &Path, host_old_root: &Path) -> Result<()> {
    let new_root = CString::new(rootfs.as_os_str().as_bytes())
        .with_context(|| format!("rootfs path contains interior NUL: {}", rootfs.display()))?;
    let put_old = CString::new(host_old_root.as_os_str().as_bytes()).with_context(|| {
        format!(
            "old root path contains interior NUL: {}",
            host_old_root.display()
        )
    })?;
    let rc = unsafe { libc::syscall(libc::SYS_pivot_root, new_root.as_ptr(), put_old.as_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "pivot_root failed to change root from '{}' to '{}'",
                rootfs.display(),
                host_old_root.display()
            )
        });
    }
    Ok(())
}

fn mount_post_pivot_filesystems(
    old_root: &Path,
    workspace_fs: &Path,
    workspace_idmap_option: &str,
    disk_backed_tmp: bool,
) -> Result<()> {
    mount_proc_if_needed()?;
    mount_devpts_if_needed()?;
    bind_sys_if_needed(old_root)?;
    mount_workspace_tmp_if_needed(
        old_root,
        workspace_fs,
        Path::new("/tmp"),
        workspace_idmap_option,
        disk_backed_tmp,
    )?;
    mount_runtime_tmpfs_if_needed(Path::new("/run/enclave/auth"))?;
    mount_runtime_tmpfs_if_needed(Path::new("/run/enclave/env"))?;
    Ok(())
}

fn mount_devpts_if_needed() -> Result<()> {
    let target = Path::new("/dev/pts");
    fs::create_dir_all(target).with_context(|| format!("failed to create {}", target.display()))?;
    if is_mountpoint(target)? {
        return Ok(());
    }
    mount(
        Some("devpts"),
        target,
        Some("devpts"),
        MsFlags::empty(),
        Option::<&str>::None,
    )
    .with_context(|| format!("failed to mount devpts at {}", target.display()))?;
    crate::perf::record_mount();
    Ok(())
}

fn mount_proc_if_needed() -> Result<()> {
    let target = Path::new("/proc");
    fs::create_dir_all(target).with_context(|| format!("failed to create {}", target.display()))?;
    let mount_result = mount(
        Some("proc"),
        target,
        Some("proc"),
        MsFlags::empty(),
        Option::<&str>::None,
    );
    match mount_result {
        Ok(()) => {
            crate::perf::record_mount();
            Ok(())
        }
        Err(err) => {
            if is_mountpoint(target).unwrap_or(false) {
                Ok(())
            } else {
                Err(err).with_context(|| format!("failed to mount proc at {}", target.display()))
            }
        }
    }
}

fn bind_sys_if_needed(old_root: &Path) -> Result<()> {
    let target = Path::new("/sys");
    fs::create_dir_all(target).with_context(|| format!("failed to create {}", target.display()))?;
    if is_mountpoint(target)? {
        return Ok(());
    }
    let source = old_root.join("sys");
    mount(
        Some(source.as_path()),
        target,
        Option::<&str>::None,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        Option::<&str>::None,
    )
    .with_context(|| {
        format!(
            "failed to bind host /sys from {} into {}",
            source.display(),
            target.display()
        )
    })?;
    crate::perf::record_mount();
    Ok(())
}

fn mount_runtime_tmpfs_if_needed(target: &Path) -> Result<()> {
    fs::create_dir_all(target).with_context(|| format!("failed to create {}", target.display()))?;
    if is_mountpoint(target)? {
        return Ok(());
    }
    mount(
        Some("tmpfs"),
        target,
        Some("tmpfs"),
        runtime_tmpfs_mount_flags(),
        Some(RUNTIME_TMPFS_DATA),
    )
    .with_context(|| format!("failed to mount tmpfs at {}", target.display()))?;
    crate::perf::record_mount();
    Ok(())
}

fn mount_workspace_tmp_if_needed(
    old_root: &Path,
    workspace_fs: &Path,
    target: &Path,
    workspace_idmap_option: &str,
    disk_backed_tmp: bool,
) -> Result<()> {
    fs::create_dir_all(target).with_context(|| format!("failed to create {}", target.display()))?;
    if is_mountpoint(target)? {
        return Ok(());
    }
    if disk_backed_tmp {
        let workspace_tmp = workspace_fs.join("tmp");
        ensure_workspace_tmp_source(old_root, &workspace_tmp)?;
        mount_workspace_source(old_root, &workspace_tmp, target, workspace_idmap_option)
            .with_context(|| {
                format!(
                    "failed to mount disk-backed workspace /tmp from {} to {}",
                    workspace_tmp.display(),
                    target.display()
                )
            })?;
        crate::perf::record_mount();
        return Ok(());
    }
    mount(
        Some("tmpfs"),
        target,
        Some("tmpfs"),
        workspace_tmp_mount_flags(),
        Some(WORKSPACE_TMP_DATA),
    )
    .with_context(|| {
        format!(
            "failed to mount private workspace tmpfs at {}",
            target.display()
        )
    })?;
    crate::perf::record_mount();
    Ok(())
}

fn runtime_tmpfs_mount_flags() -> MsFlags {
    MsFlags::MS_NODEV | MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC
}

const WORKSPACE_TMP_DATA: &str = "mode=1777";

fn workspace_tmp_mount_flags() -> MsFlags {
    MsFlags::MS_NODEV | MsFlags::MS_NOSUID
}

fn ensure_workspace_tmp_source(old_root: &Path, workspace_tmp: &Path) -> Result<()> {
    let source = path_inside_old_root(old_root, workspace_tmp)?;
    fs::create_dir_all(&source)
        .with_context(|| format!("failed to create {}", source.display()))?;
    fs::set_permissions(&source, fs::Permissions::from_mode(0o1777))
        .with_context(|| format!("failed to chmod {}", source.display()))?;
    Ok(())
}

fn path_inside_old_root(old_root: &Path, absolute_path: &Path) -> Result<PathBuf> {
    if !absolute_path.is_absolute() {
        bail!("path must be absolute: {}", absolute_path.display());
    }
    let relative = absolute_path
        .strip_prefix("/")
        .expect("absolute path strips leading slash");
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::CurDir | Component::Prefix(_)
        )
    }) {
        bail!(
            "path must not contain traversal components: {}",
            absolute_path.display()
        );
    }
    Ok(old_root.join(relative))
}

fn is_mountpoint(path: &Path) -> Result<bool> {
    let raw = match fs::read_to_string("/proc/self/mountinfo") {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).context("failed to read /proc/self/mountinfo"),
    };
    let needle = path.to_string_lossy();
    for line in raw.lines() {
        let mut fields = line.split_whitespace();
        let _mount_id = fields.next();
        let _parent_id = fields.next();
        let _major_minor = fields.next();
        let _root = fields.next();
        let Some(mount_point) = fields.next() else {
            continue;
        };
        if mount_point == needle {
            return Ok(true);
        }
    }
    Ok(false)
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
#[path = "../../tests/src/commands/internal.rs"]
mod tests;
