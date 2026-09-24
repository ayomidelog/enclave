use super::*;

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

pub(crate) fn workspace_old_root_path(rootfs: &Path, workspace_id: &str) -> Result<PathBuf> {
    Ok(rootfs.join(workspace_old_root_name(workspace_id)?))
}

pub(crate) fn workspace_old_root_name(workspace_id: &str) -> Result<String> {
    if workspace_id.is_empty()
        || !workspace_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        bail!("workspace id is unsafe for old-root mount path: {workspace_id}");
    }
    Ok(format!(".old_root-{workspace_id}"))
}

pub(crate) fn mount_workspace_root_overlay(
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

pub(crate) fn overlay_mount_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\134")
        .replace(',', "\\054")
}

pub(crate) fn validate_workspace_overlay_path(raw: &str, label: &str) -> Result<PathBuf> {
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

pub(crate) fn open_ready_file_via_old_root(old_root: &Path, ready_file: &Path) -> Result<File> {
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

pub(crate) fn validate_workspace_rootfs(rootfs: &Path) -> Result<PathBuf> {
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

pub(crate) fn bind_mount_self(path: &Path) -> Result<()> {
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

pub(crate) fn pivot_into_rootfs(rootfs: &Path, host_old_root: &Path) -> Result<()> {
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

pub(crate) fn path_inside_old_root(old_root: &Path, absolute_path: &Path) -> Result<PathBuf> {
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
