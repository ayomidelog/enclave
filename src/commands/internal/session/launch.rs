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
                ForkResult::Child => exec_workspace_session_init(&args),
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

/// Become the in-namespace session init, which finishes the setup in this process.
///
/// The work that used to happen in a shell script runs here instead: one exec of
/// our own binary replaces the shell plus the mount, hostname, and readlink
/// processes it spawned, all of which were on the workspace start critical path.
pub(crate) fn exec_workspace_session_init(args: &WorkspaceSessionLaunchArgs) -> Result<()> {
    let mut command = Command::new(&args.session_helper);
    command
        .arg("internal")
        .arg("workspace-session-init")
        .arg("--rootfs")
        .arg(&args.rootfs)
        .arg("--workspace-fs")
        .arg(&args.workspace_fs)
        .arg("--workspace-id")
        .arg(&args.workspace_id)
        .arg("--mount-target")
        .arg(&args.mount_target)
        .arg("--mount-ref")
        .arg(&args.mount_ref)
        .arg("--pid-ref")
        .arg(&args.pid_ref)
        .arg("--pid-file")
        .arg(&args.pid_file)
        .arg("--ready-file")
        .arg(&args.ready_file)
        .arg("--workspace-hostname")
        .arg(&args.workspace_hostname)
        .arg("--session-helper")
        .arg(&args.session_helper)
        .arg("--apparmor-profile")
        .arg(&args.apparmor_profile)
        .arg("--selinux-label")
        .arg(&args.selinux_label)
        .arg("--workspace-idmap-option")
        .arg(&args.workspace_idmap_option)
        .arg("--root-overlay-upper")
        .arg(&args.root_overlay_upper)
        .arg("--root-overlay-work")
        .arg(&args.root_overlay_work)
        .arg("--root-overlay-merged")
        .arg(&args.root_overlay_merged);
    for (flag, value) in [
        ("--cpu-limit", Some(args.cpu_limit.as_str())),
        ("--memory-limit-kb", Some(args.memory_limit_kb.as_str())),
        ("--proc-limit", Some(args.proc_limit.as_str())),
        ("--nofile-limit", Some(args.nofile_limit.as_str())),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            command.arg(flag).arg(value);
        }
    }
    if args.disk_backed_tmp {
        command.arg("--disk-backed-tmp");
    }

    let err = command.exec();
    Err(err).context("failed to exec the workspace session init")
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
