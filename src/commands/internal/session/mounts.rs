use super::*;

use crate::hostcmd::HostCommand;

pub(crate) fn mount_workspace_source(
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
    // `Path::exists` reports false for every lookup failure, which hides whether
    // the path is missing or merely unreachable, and the old-root path alone does
    // not tell the operator which host path to inspect.
    match fs::symlink_metadata(&source) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => bail!(
            "workspace source path is not a directory inside the workspace session: {} (host path {})",
            source.display(),
            workspace_fs.display()
        ),
        Err(error) => bail!(
            "workspace source path is unavailable inside the workspace session: {} (host path {}, error: {error})",
            source.display(),
            workspace_fs.display()
        ),
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
        let status = HostCommand::new(mount_binary)
            .arg("--bind")
            .arg("-o")
            .arg(format!("X-mount.idmap={workspace_idmap_option}"))
            .arg(&source)
            .arg(mount_target)
            .discard_output()
            .run()
            .with_context(|| format!("failed to execute {}", mount_binary.display()))?;
        if status.success() {
            return Ok(());
        }
        bail!(
            "idmapped workspace bind mount failed via {} with status {}",
            mount_binary.display(),
            status.status
        );
    }

    bail!("idmapped workspace bind mount requires /bin/mount or /usr/bin/mount inside the rootfs");
}

pub(crate) fn mount_post_pivot_filesystems(
    old_root: &Path,
    workspace_fs: &Path,
    workspace_idmap_option: &str,
    disk_backed_tmp: bool,
) -> Result<()> {
    mount_proc_if_needed()?;
    let devpts_timer = crate::perf::Timer::new("session.mounts.devpts");
    mount_devpts_if_needed()?;
    drop(devpts_timer);
    let sys_timer = crate::perf::Timer::new("session.mounts.sys");
    bind_sys_if_needed(old_root)?;
    drop(sys_timer);
    let tmp_timer = crate::perf::Timer::new("session.mounts.tmp");
    mount_workspace_tmp_if_needed(
        old_root,
        workspace_fs,
        Path::new("/tmp"),
        workspace_idmap_option,
        disk_backed_tmp,
    )?;
    drop(tmp_timer);
    let runtime_timer = crate::perf::Timer::new("session.mounts.runtime");
    mount_runtime_tmpfs_if_needed(Path::new("/run/enclave/auth"))?;
    mount_runtime_tmpfs_if_needed(Path::new("/run/enclave/env"))?;
    drop(runtime_timer);
    Ok(())
}

pub(crate) fn mount_devpts_if_needed() -> Result<()> {
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

pub(crate) fn mount_proc_if_needed() -> Result<()> {
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

pub(crate) fn bind_sys_if_needed(old_root: &Path) -> Result<()> {
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

pub(crate) fn mount_runtime_tmpfs_if_needed(target: &Path) -> Result<()> {
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

pub(crate) fn is_mountpoint(path: &Path) -> Result<bool> {
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
