use super::*;

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

pub(crate) fn unshare_workspace_namespaces(enable_userns: bool) -> Result<()> {
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

pub(crate) fn wait_for_child_unshare(sync: &mut UnixStream) -> Result<()> {
    let mut ready = [0u8; 1];
    sync.read_exact(&mut ready)
        .context("workspace launcher child exited before namespace setup completed")
}

pub(crate) fn apply_workspace_id_maps(pid: u32, args: &WorkspaceSessionLaunchArgs) -> Result<()> {
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

pub(crate) fn finalize_workspace_identity() -> Result<()> {
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

pub(crate) fn write_id_map(path: &str, inner: u32, outer: u32, count: u32) -> Result<()> {
    write_proc_file(path, &format!("{inner} {outer} {count}\n"))
}

pub(crate) fn write_proc_file(path: &str, contents: &str) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("failed to write {}", path))
}

pub(crate) fn exec_workspace_session_script(args: &WorkspaceSessionLaunchArgs) -> Result<()> {
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

pub(crate) fn run_workspace_session_loop(args: WorkspaceSessionLoopArgs) -> Result<()> {
    run_workspace_session_loop_inner(Path::new(&args.old_root), Path::new(&args.ready_file))
}

pub(crate) fn run_workspace_session_loop_inner(old_root: &Path, ready_file: &Path) -> Result<()> {
    let ready_handle = open_ready_file_via_old_root(old_root, ready_file)?;
    crate::workspace::session::mask_runtime_paths()?;
    crate::workspace::session::tighten_namespace_mounts()?;
    crate::workspace::session::detach_old_root(old_root)?;
    crate::workspace::session::apply_session_restrictions()?;
    signal_ready(ready_handle)?;
    crate::commands::internal::runtime_init::run_runtime_init_loop()
}

pub(crate) fn signal_ready(mut ready_handle: File) -> Result<()> {
    ready_handle
        .write_all(b"ready\n")
        .context("failed to write ready marker")?;
    ready_handle
        .flush()
        .context("failed to flush ready marker")?;
    Ok(())
}
