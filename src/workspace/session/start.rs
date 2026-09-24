use super::*;

pub fn start_session(
    workspace: &WorkspaceMetadata,
    apparmor_profile: Option<&str>,
    selinux_label: Option<&str>,
) -> Result<SessionInfo> {
    // This phase is on the user-visible startup critical path, so break it into
    // staging, launch, and readiness instead of reporting one number.
    let staging = crate::perf::Timer::new("session.staging");
    ensure_runtime_layout(workspace)?;
    let userns = detect_user_namespace_mode()?;
    let current_exe = prepare_session_helper(workspace)?;
    drop(staging);
    let workspace_source = workspace
        .home_mount_source_path
        .as_deref()
        .unwrap_or(&workspace.filesystem_path);
    let workspace_bind_idmap = match &userns {
        UserNamespaceMode::Enabled(plan) => {
            workspace_bind_mount_idmap_option(Path::new(workspace_source), plan).with_context(
                || {
                    format!(
                        "failed to derive idmapped bind mount option for workspace source {}",
                        workspace_source
                    )
                },
            )?
        }
        UserNamespaceMode::Disabled => None,
    };

    let (mount_ref_path, pid_ref_path) = namespace_ref_paths(workspace);
    let pid_file = runtime_pid_file(workspace);
    let ready_file = runtime_ready_file(workspace);
    let log_file = runtime_log_file(workspace);

    if let Err(err) = fs::remove_file(&pid_file) {
        if err.kind() != std::io::ErrorKind::NotFound {
            return Err(err).with_context(|| format!("failed to remove {}", pid_file.display()));
        }
    }
    if let Err(err) = fs::remove_file(&ready_file) {
        if err.kind() != std::io::ErrorKind::NotFound {
            return Err(err).with_context(|| format!("failed to remove {}", ready_file.display()));
        }
    }

    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .with_context(|| format!("failed to open {}", log_file.display()))?;
    let stderr = stdout
        .try_clone()
        .with_context(|| format!("failed to clone {}", log_file.display()))?;

    let disk_backed_tmp = crate::workspace::storage::workspace_uses_disk_image(workspace);
    let root_overlay_paths = crate::workspace::storage::root_overlay_paths(workspace);
    let mut command = Command::new("setsid");
    command
        .arg("-f")
        .arg(&current_exe)
        .arg("internal")
        .arg("workspace-session-launch")
        .args(launch_userns_args(&userns))
        .arg("--rootfs")
        .arg(&workspace.sandbox_rootfs_path)
        .arg("--workspace-fs")
        .arg(workspace_source)
        .arg("--workspace-id")
        .arg(&workspace.id)
        .arg("--mount-target")
        .arg(&workspace.filesystem_mount_target)
        .arg("--mount-ref")
        .arg(&mount_ref_path)
        .arg("--pid-ref")
        .arg(&pid_ref_path)
        .arg("--pid-file")
        .arg(&pid_file)
        .arg("--ready-file")
        .arg(&ready_file)
        .arg("--cpu-limit")
        .arg(
            workspace
                .limits
                .cpu_seconds
                .map(|v| v.to_string())
                .unwrap_or_default(),
        )
        .arg("--memory-limit-kb")
        .arg(
            workspace
                .limits
                .memory_bytes
                .map(|v| (v / 1024).to_string())
                .unwrap_or_default(),
        )
        .arg("--proc-limit")
        .arg(
            workspace
                .limits
                .max_processes
                .map(|v| v.to_string())
                .unwrap_or_default(),
        )
        .arg("--nofile-limit")
        .arg(
            workspace
                .limits
                .max_open_files
                .map(|v| v.to_string())
                .unwrap_or_default(),
        )
        .arg("--workspace-hostname")
        .arg(process::workspace_runtime_hostname(&workspace.name))
        .arg("--session-helper")
        .arg(&current_exe)
        .arg("--apparmor-profile")
        .arg(apparmor_profile.unwrap_or_default())
        .arg("--selinux-label")
        .arg(selinux_label.unwrap_or_default())
        .arg("--workspace-idmap-option")
        .arg(workspace_bind_idmap.unwrap_or_default());
    if disk_backed_tmp {
        command.arg("--disk-backed-tmp");
    }
    if let Some((upper, work, merged)) = root_overlay_paths {
        command
            .arg("--root-overlay-upper")
            .arg(upper)
            .arg("--root-overlay-work")
            .arg(work)
            .arg("--root-overlay-merged")
            .arg(merged);
    }
    let launch = crate::perf::Timer::new("session.launch");
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status()
        .context("failed to launch workspace session via setsid/unshare")?;
    drop(launch);

    if !status.success() {
        bail!("failed to launch workspace session (status {status})");
    }

    let ready = crate::perf::Timer::new("session.ready");
    wait_for_session_ready(&ready_file, &pid_file, &log_file, START_TIMEOUT)?;
    drop(ready);
    let pid = process::read_pid_file(&pid_file)?;
    if !process_alive(pid) {
        let tail = process::read_log_tail(&log_file, 20).unwrap_or_default();
        let rendered_tail = if tail.trim().is_empty() {
            "<empty>".to_string()
        } else {
            tail
        };
        bail!(
            "workspace session pid {} exited before startup completed. log file: {}. recent log:\n{}",
            pid,
            log_file.display(),
            rendered_tail
        );
    }

    let starttime_ticks = process_starttime_ticks(pid)?;
    let (mount_ns, pid_ns) = read_namespace_refs(pid)?;
    Ok(SessionInfo {
        pid,
        starttime_ticks,
        mount_ns,
        pid_ns,
    })
}

pub(crate) fn wait_for_session_ready(
    ready_file: &Path,
    pid_file: &Path,
    log_file: &Path,
    timeout: Duration,
) -> Result<()> {
    let parent = ready_file
        .parent()
        .ok_or_else(|| anyhow::anyhow!("session ready path has no parent"))?;
    let parent_cstr = std::ffi::CString::new(parent.as_os_str().as_bytes())
        .context("session ready path contains an interior NUL byte")?;
    let inotify_fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
    if inotify_fd < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to initialize inotify");
    }
    let watch = unsafe {
        libc::inotify_add_watch(
            inotify_fd,
            parent_cstr.as_ptr(),
            libc::IN_CLOSE_WRITE | libc::IN_CREATE | libc::IN_MOVED_TO,
        )
    };
    if watch < 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(inotify_fd) };
        return Err(error).context("failed to watch session readiness directory");
    }

    let started = Instant::now();
    let mut events = [0u8; 4096];
    loop {
        if ready_file.exists() && pid_file.exists() {
            unsafe {
                libc::inotify_rm_watch(inotify_fd, watch);
                libc::close(inotify_fd);
            }
            return Ok(());
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            unsafe {
                libc::inotify_rm_watch(inotify_fd, watch);
                libc::close(inotify_fd);
            }
            let tail = process::read_log_tail(log_file, 20).unwrap_or_default();
            let rendered_tail = if tail.trim().is_empty() {
                "<empty>".to_string()
            } else {
                tail
            };
            bail!(
                "workspace session did not become ready within {}s (expected files: {}, {}). log file: {}. recent log:\n{}",
                timeout.as_secs(),
                pid_file.display(),
                ready_file.display(),
                log_file.display(),
                rendered_tail
            );
        }

        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: inotify_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let poll_result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if poll_result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            unsafe {
                libc::inotify_rm_watch(inotify_fd, watch);
                libc::close(inotify_fd);
            }
            return Err(error).context("failed waiting for session readiness");
        }
        if poll_result > 0 {
            let _ = unsafe { libc::read(inotify_fd, events.as_mut_ptr().cast(), events.len()) };
        }
    }
}

pub(crate) fn setgroups_args(userns: &userns::UserNamespacePlan) -> Vec<&'static str> {
    if userns.gid_map.count > 1 {
        vec!["--deny-setgroups"]
    } else {
        Vec::new()
    }
}

pub(crate) fn launch_userns_args(userns: &UserNamespaceMode) -> Vec<String> {
    match userns {
        UserNamespaceMode::Enabled(plan) => {
            let mut args = vec![
                "--enable-userns".to_string(),
                "--uid-inner".to_string(),
                plan.uid_map.inner_start.to_string(),
                "--uid-outer".to_string(),
                plan.uid_map.outer_start.to_string(),
                "--uid-count".to_string(),
                plan.uid_map.count.to_string(),
                "--gid-inner".to_string(),
                plan.gid_map.inner_start.to_string(),
                "--gid-outer".to_string(),
                plan.gid_map.outer_start.to_string(),
                "--gid-count".to_string(),
                plan.gid_map.count.to_string(),
            ];
            args.extend(setgroups_args(plan).into_iter().map(String::from));
            args
        }
        UserNamespaceMode::Disabled => vec![
            "--uid-inner".to_string(),
            "0".to_string(),
            "--uid-outer".to_string(),
            "0".to_string(),
            "--uid-count".to_string(),
            "1".to_string(),
            "--gid-inner".to_string(),
            "0".to_string(),
            "--gid-outer".to_string(),
            "0".to_string(),
            "--gid-count".to_string(),
            "1".to_string(),
        ],
    }
}
