use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use nix::mount::{mount, MsFlags};
use nix::sched::{setns, CloneFlags};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{fork, ForkResult, Pid};

use crate::cli::{
    WorkspaceCommandInternalArgs, WorkspaceSessionBootstrapArgs, WorkspaceSessionLaunchArgs,
    WorkspaceSessionLoopArgs,
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
    let host_old_root = rootfs.join(".old_root");

    fs::create_dir_all(&host_old_root)
        .with_context(|| format!("failed to create {}", host_old_root.display()))?;
    bind_mount_self(&rootfs)?;
    pivot_into_rootfs(&rootfs, &host_old_root)?;
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

pub(crate) fn run_workspace_session_loop(args: WorkspaceSessionLoopArgs) -> Result<()> {
    run_workspace_session_loop_inner(Path::new(&args.old_root), Path::new(&args.ready_file))
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

    let namespaces = NamespaceHandles::open(args.runtime_pid)?;
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

struct NamespaceHandles {
    root_dir: File,
    user_ns: File,
    mount_ns: File,
    pid_ns: File,
    net_ns: File,
    uts_ns: File,
}

impl NamespaceHandles {
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
    .with_context(|| format!("failed to bind-mount {}", path.display()))
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
    .with_context(|| format!("failed to mount devpts at {}", target.display()))
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
        Ok(()) => Ok(()),
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
    })
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
    .with_context(|| format!("failed to mount tmpfs at {}", target.display()))
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
        return mount_workspace_source(old_root, &workspace_tmp, target, workspace_idmap_option)
            .with_context(|| {
                format!(
                    "failed to mount disk-backed workspace /tmp from {} to {}",
                    workspace_tmp.display(),
                    target.display()
                )
            });
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
    })
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
