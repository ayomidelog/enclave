//! In-namespace session setup, between the launcher and the bootstrap helper.
//!
//! The launcher unshares the workspace namespaces and then hands off to this
//! step, which runs inside them. It applies the workspace resource limits, makes
//! the mount tree private, sets the hostname, records the namespace references
//! and the host pid, and then becomes the hardened bootstrap helper.
//!
//! This used to be a shell script. Every step the script could not do itself —
//! the private mount, the hostname, each namespace reference — was a process, and
//! those processes were on the critical path of every workspace start. Doing the
//! same work in this process removes them without changing what happens.

use super::*;

/// Longest hostname the kernel accepts, including the terminator.
const HOSTNAME_LIMIT: usize = 64;

pub(crate) fn run_workspace_session_init(args: WorkspaceSessionInitArgs) -> Result<()> {
    log("workspace session bootstrap starting");
    log(&format!("rootfs={}", args.rootfs));
    log(&format!("workspace_fs={}", args.workspace_fs));
    log(&format!("root_overlay_upper={}", args.root_overlay_upper));
    log(&format!("root_overlay_work={}", args.root_overlay_work));
    log(&format!("root_overlay_merged={}", args.root_overlay_merged));

    // These steps run between the namespace unshare and the bootstrap helper, so
    // they are inside the readiness wait the daemon is blocked on.
    apply_workspace_limits(&args);
    make_mounts_private()?;
    set_workspace_hostname(&args.workspace_hostname);
    let refs = crate::perf::Timer::new("session.init.namespace_refs");
    write_namespace_references(&args.mount_ref, &args.pid_ref)?;
    write_host_pid(&args.pid_file)?;
    drop(refs);

    log("workspace session bootstrap complete; switching to hardened runtime helper");
    exec_bootstrap_helper(&args)
}

/// Apply the workspace limits this process and its descendants inherit.
///
/// A limit the kernel refuses is reported and otherwise ignored, which is what
/// the shell script did: the cgroup is the enforcement that matters, and these
/// limits are a second line of defence that must not stop a start.
fn apply_workspace_limits(args: &WorkspaceSessionInitArgs) {
    set_limit(
        libc::RLIMIT_CPU,
        parse_limit(&args.cpu_limit, "max CPU seconds"),
        false,
        "max CPU seconds",
    );
    set_limit(
        libc::RLIMIT_AS,
        parse_limit(&args.memory_limit_kb, "max memory").and_then(|kb| kb.checked_mul(1024)),
        false,
        "max memory",
    );
    set_limit(
        libc::RLIMIT_NOFILE,
        parse_limit(&args.nofile_limit, "max open files"),
        false,
        "max open files",
    );
    // The process count limit is set on both the soft and the hard limit, which
    // is what the previous implementation's prlimit call did.
    set_limit(
        libc::RLIMIT_NPROC,
        parse_limit(&args.proc_limit, "max processes"),
        true,
        "max processes",
    );
}

/// Read a limit the launcher passed as text.
///
/// An empty value means the workspace did not configure that limit. A value the
/// launcher would not have produced is reported and treated as unset rather than
/// failing a start over a secondary limit.
fn parse_limit(raw: &str, what: &str) -> Option<u64> {
    if raw.is_empty() {
        return None;
    }
    match raw.parse::<u64>() {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!("ignoring an unreadable {what} limit {raw:?}: {error}");
            None
        }
    }
}

fn set_limit(resource: libc::__rlimit_resource_t, value: Option<u64>, also_hard: bool, what: &str) {
    let Some(value) = value else {
        return;
    };
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(resource, &mut current) } != 0 {
        tracing::warn!("failed to read the current {what} limit");
        return;
    }
    let next = libc::rlimit {
        rlim_cur: value,
        rlim_max: if also_hard { value } else { current.rlim_max },
    };
    if unsafe { libc::setrlimit(resource, &next) } != 0 {
        let error = std::io::Error::last_os_error();
        tracing::warn!("failed to set {what} to {value}: {error}");
    }
}

/// Stop this namespace's mount tree from propagating changes to the host.
///
/// This has to happen before anything else mounts, and it is the step that keeps
/// a workspace from altering the host mount tree.
fn make_mounts_private() -> Result<()> {
    let target = CString::new("/").expect("literal contains no nul");
    let rc = unsafe {
        libc::mount(
            std::ptr::null(),
            target.as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to make the workspace mount tree private");
    }
    Ok(())
}

/// Give the workspace its own hostname.
///
/// A kernel that refuses to set one is not a reason to fail a start, so a failure
/// is reported and the start continues.
fn set_workspace_hostname(hostname: &str) {
    let Some(name) = hostname_to_apply(hostname) else {
        tracing::warn!(
            "workspace hostname is longer than the kernel accepts ({} bytes); leaving it unchanged",
            hostname.len()
        );
        return;
    };
    let rc = unsafe { libc::sethostname(name.as_ptr().cast(), name.len()) };
    if rc != 0 {
        let error = std::io::Error::last_os_error();
        tracing::warn!("failed to set workspace hostname to {name}: {error}");
    }
}

/// The hostname to set, or nothing when it cannot be set at all.
///
/// An unnamed workspace gets a fixed placeholder rather than the host's name, and
/// a name the kernel would reject is refused here instead of being truncated into
/// something an operator would not recognise.
fn hostname_to_apply(configured: &str) -> Option<&str> {
    let name = if configured.is_empty() {
        "workspace"
    } else {
        configured
    };
    (name.len() < HOSTNAME_LIMIT).then_some(name)
}

/// Record which mount and pid namespaces this runtime is in.
///
/// The daemon reads these back to enter the workspace later, so a failure here is
/// fatal: a runtime whose namespaces cannot be found cannot be entered, cannot be
/// stopped with identity proof, and cannot be cleaned up by name.
fn write_namespace_references(mount_ref: &str, pid_ref: &str) -> Result<()> {
    for (path, source) in [
        (mount_ref, "/proc/self/ns/mnt"),
        (pid_ref, "/proc/self/ns/pid"),
    ] {
        let target = fs::read_link(source)
            .with_context(|| format!("failed to read the workspace namespace at {source}"))?;
        let rendered = format!("{}\n", target.to_string_lossy());
        fs::write(path, rendered).with_context(|| format!("failed to write {path}"))?;
    }
    Ok(())
}

/// Record the pid this runtime has in the host pid namespace.
///
/// Inside the workspace the runtime is pid 1, so the host pid has to be read from
/// the NSpid field rather than from getpid. The daemon uses it to identify the
/// runtime, so a missing value fails the start.
fn write_host_pid(pid_file: &str) -> Result<()> {
    let status = fs::read_to_string("/proc/self/status")
        .context("failed to read /proc/self/status for the workspace host pid")?;
    let host_pid = status
        .lines()
        .find_map(|line| {
            line.strip_prefix("NSpid:")?
                .split_whitespace()
                .next()
                .map(str::to_string)
        })
        .ok_or_else(|| anyhow::anyhow!("failed to resolve host pid from /proc/self/status"))?;
    let rendered = format!("{host_pid}\n");
    fs::write(pid_file, rendered).with_context(|| format!("failed to write {pid_file}"))
}

/// Become the hardened bootstrap helper.
///
/// The helper runs as the workspace runtime and does the mounts and the pivot, so
/// it is the last thing this process does. When an LSM profile is configured the
/// helper is entered through setpriv, which is the only way to apply it at exec.
fn exec_bootstrap_helper(args: &WorkspaceSessionInitArgs) -> Result<()> {
    let mut command = if args.apparmor_profile.is_empty() && args.selinux_label.is_empty() {
        Command::new(&args.session_helper)
    } else {
        let mut command = Command::new("setpriv");
        command.arg("--nnp");
        if !args.apparmor_profile.is_empty() {
            command.arg(format!("--apparmor-profile={}", args.apparmor_profile));
        }
        if !args.selinux_label.is_empty() {
            command.arg(format!("--selinux-label={}", args.selinux_label));
        }
        command.arg(&args.session_helper);
        command
    };

    command
        .arg("internal")
        .arg("workspace-session-bootstrap")
        .arg("--rootfs")
        .arg(&args.rootfs)
        .arg("--workspace-fs")
        .arg(&args.workspace_fs)
        .arg("--workspace-id")
        .arg(&args.workspace_id)
        .arg("--mount-target")
        .arg(&args.mount_target)
        .arg("--workspace-idmap-option")
        .arg(&args.workspace_idmap_option)
        .arg("--ready-file")
        .arg(&args.ready_file);
    if args.disk_backed_tmp {
        command.arg("--disk-backed-tmp");
    }
    if !args.root_overlay_upper.is_empty() {
        command
            .arg("--root-overlay-upper")
            .arg(&args.root_overlay_upper)
            .arg("--root-overlay-work")
            .arg(&args.root_overlay_work)
            .arg("--root-overlay-merged")
            .arg(&args.root_overlay_merged);
    }

    let err = command.exec();
    Err(err).context("failed to exec the workspace session bootstrap helper")
}

fn log(message: &str) {
    eprintln!("{message}");
}

#[cfg(test)]
#[path = "../../../../tests/src/commands/internal/session/init.rs"]
mod tests;
