use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::unistd::{fork, ForkResult};

use crate::cli::{
    WorkspaceSessionBootstrapArgs, WorkspaceSessionLaunchArgs, WorkspaceSessionLoopArgs,
};

pub(super) const RUNTIME_TMPFS_DATA: &str = "mode=700";

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
    let old_root_name = workspace_old_root_name(&args.workspace_id)?;
    let pivoted_old_root = PathBuf::from("/").join(&old_root_name);
    let (new_root, host_old_root) = if args.root_overlay_merged.is_empty() {
        let host_old_root = workspace_old_root_path(&rootfs, &args.workspace_id)?;
        fs::create_dir_all(&host_old_root)
            .with_context(|| format!("failed to create {}", host_old_root.display()))?;
        bind_mount_self(&rootfs)?;
        (rootfs, host_old_root)
    } else {
        let upper = validate_workspace_overlay_path(&args.root_overlay_upper, "upper")?;
        let work = validate_workspace_overlay_path(&args.root_overlay_work, "work")?;
        let merged = validate_workspace_overlay_path(&args.root_overlay_merged, "merged")?;
        mount_workspace_root_overlay(&rootfs, &upper, &work, &merged)?;
        let host_old_root = workspace_old_root_path(&merged, &args.workspace_id)?;
        fs::create_dir_all(&host_old_root)
            .with_context(|| format!("failed to create {}", host_old_root.display()))?;
        (merged, host_old_root)
    };

    pivot_into_rootfs(&new_root, &host_old_root)?;
    std::env::set_current_dir("/").context("failed to chdir to / after pivot_root")?;
    mount_workspace_source(
        &pivoted_old_root,
        Path::new(&args.workspace_fs),
        Path::new(&args.mount_target),
        &args.workspace_idmap_option,
    )?;
    mount_post_pivot_filesystems(
        &pivoted_old_root,
        Path::new(&args.workspace_fs),
        &args.workspace_idmap_option,
        args.disk_backed_tmp,
    )?;
    run_workspace_session_loop_inner(&pivoted_old_root, Path::new(&args.ready_file))
}

pub(super) fn workspace_old_root_path(rootfs: &Path, workspace_id: &str) -> Result<PathBuf> {
    Ok(rootfs.join(workspace_old_root_name(workspace_id)?))
}

fn workspace_old_root_name(workspace_id: &str) -> Result<String> {
    if workspace_id.is_empty()
        || !workspace_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        bail!("workspace id is unsafe for old-root mount path: {workspace_id}");
    }
    Ok(format!(".old_root-{workspace_id}"))
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

pub(super) fn open_ready_file_via_old_root(old_root: &Path, ready_file: &Path) -> Result<File> {
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
        .arg(&args.workspace_id)
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
    if disk_backed_tmp {
        let workspace_tmp = workspace_fs.join("tmp");
        ensure_workspace_tmp_source(old_root, &workspace_tmp)?;
        let source = path_inside_old_root(old_root, &workspace_tmp)?;
        let source_identity = directory_identity(&source)?;
        if is_mountpoint(target)? {
            let current_is_expected = directory_identity(target)
                .is_ok_and(|identity| identity == source_identity)
                && tmp_directory_is_usable(target);
            if current_is_expected {
                return Ok(());
            }
            umount2(target, MntFlags::MNT_DETACH).with_context(|| {
                format!(
                    "failed to detach stale workspace /tmp mount at {}",
                    target.display()
                )
            })?;
        }
        mount_workspace_source(old_root, &workspace_tmp, target, workspace_idmap_option)
            .with_context(|| {
                format!(
                    "failed to mount disk-backed workspace /tmp from {} to {}",
                    workspace_tmp.display(),
                    target.display()
                )
            })?;
        crate::perf::record_mount();
        return verify_workspace_tmp_mount(target, Some(source_identity));
    }
    if is_mountpoint(target)? {
        if tmp_directory_is_usable(target) {
            return Ok(());
        }
        umount2(target, MntFlags::MNT_DETACH).with_context(|| {
            format!(
                "failed to detach stale workspace /tmp mount at {}",
                target.display()
            )
        })?;
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
    verify_workspace_tmp_mount(target, None)
}

pub(super) fn runtime_tmpfs_mount_flags() -> MsFlags {
    MsFlags::MS_NODEV | MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC
}

pub(super) const WORKSPACE_TMP_DATA: &str = "mode=1777";

fn workspace_tmp_mount_flags() -> MsFlags {
    MsFlags::MS_NODEV | MsFlags::MS_NOSUID
}

fn ensure_workspace_tmp_source(old_root: &Path, workspace_tmp: &Path) -> Result<()> {
    let source = path_inside_old_root(old_root, workspace_tmp)?;
    fs::create_dir_all(&source)
        .with_context(|| format!("failed to create {}", source.display()))?;
    let metadata = fs::symlink_metadata(&source).with_context(|| {
        format!(
            "failed to inspect workspace tmp source {}",
            source.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "workspace tmp source must be a real directory: {}",
            source.display()
        );
    }
    fs::set_permissions(&source, fs::Permissions::from_mode(0o1777))
        .with_context(|| format!("failed to chmod {}", source.display()))?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DirectoryIdentity {
    pub(super) device: u64,
    pub(super) inode: u64,
}

fn directory_identity(path: &Path) -> Result<DirectoryIdentity> {
    let metadata = fs::metadata(path).with_context(|| {
        format!(
            "failed to inspect workspace tmp directory {}",
            path.display()
        )
    })?;
    if !metadata.is_dir() {
        bail!("workspace tmp path is not a directory: {}", path.display());
    }
    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

pub(super) fn tmp_directory_is_usable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_dir() || metadata.nlink() < 2 || metadata.mode() & 0o1777 != 0o1777 {
        return false;
    }
    let probe = path.join(format!(
        ".enclave-tmp-probe-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    match OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(file) => {
            drop(file);
            fs::remove_file(probe).is_ok()
        }
        Err(_) => false,
    }
}

pub(super) fn verify_workspace_tmp_mount(
    target: &Path,
    expected_identity: Option<DirectoryIdentity>,
) -> Result<()> {
    let identity = directory_identity(target)?;
    if expected_identity.is_some_and(|expected| expected != identity) {
        bail!(
            "workspace /tmp mount at {} does not reference its configured backing directory",
            target.display()
        );
    }
    if !tmp_directory_is_usable(target) {
        bail!(
            "workspace /tmp at {} is not a linked, writable directory with mode 1777",
            target.display()
        );
    }
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
