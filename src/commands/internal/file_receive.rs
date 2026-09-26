use super::*;

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

pub(crate) fn validate_workspace_file_target(target: &str) -> Result<()> {
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
