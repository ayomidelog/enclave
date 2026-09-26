//! Starting one workspace session.
//!
//! A session is launched as a detached process that sets up its own namespaces,
//! mounts, and hardening, and then writes a pid file and a ready file. This
//! module owns the launch and the order it runs in; waiting for those two files
//! is the readiness module, and the user-namespace arguments the launcher is
//! given are in the userns_args module.

mod launch;
mod readiness;
mod reap;
mod userns_args;

use super::*;

use launch::launch_until_ready;
use reap::reap_launched_session;

/// How many times a launch that failed on a busy helper binary is retried.
const LAUNCH_ATTEMPTS: usize = 3;

/// Whether the session log ends in the kernel's busy-executable error.
///
/// The launcher is started with setsid -f, so its own exit status says nothing
/// about whether the helper could be executed: the failure is written to the log
/// by setsid. Only this specific failure is retried, so a launch that fails for
/// any other reason is reported as it is rather than repeated.
pub(crate) fn log_reports_text_file_busy(log_file: &Path) -> bool {
    let Ok(tail) = process::read_log_tail(log_file, 20) else {
        return false;
    };
    tail.contains("Text file busy")
}

// Used by the launch below, and by the tests through the session module.
pub(crate) use userns_args::launch_userns_args;

// The session tests reach the readiness diagnostic and the user-namespace
// argument builders directly, because each is what the launch depends on and
// each is testable without a real namespace.
#[cfg(test)]
pub(crate) use readiness::session_helper_load_failure;
#[cfg(test)]
pub(crate) use userns_args::setgroups_args;

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
    // The helper binary is installed by copying it into the sandbox runtime
    // directory and hard-linking it into place, and the copy holds the
    // destination open for writing. A descriptor is closed when a child execs,
    // not when it forks, so a fork in another thread between the copy and the
    // child's own exec leaves a writer on that inode for as long as the fork
    // takes to exec. An exec of the helper in that window fails with the kernel's
    // busy-executable error. It is transient by construction, and under load the
    // fork-to-exec window is wide enough to hit it.
    //
    // The launcher is started with setsid -f, which exits as soon as it has
    // forked, so that failure is not visible in the exit status: it surfaces as
    // the session never becoming ready, with the reason in the session log. Only
    // that reason is retried, so a launch that failed for anything else is
    // reported as it is. The retry is bounded and recorded.
    //
    // Every failure after this point may have left a session running, and the
    // caller has no pid to stop it by, so the launch is one fallible step whose
    // error path reaps what it started. Without that, a launch that timed out while
    // the session was still setting up becomes a live runtime that nothing records.
    let outcome = launch_until_ready(
        workspace,
        &mut command,
        &log_file,
        &pid_file,
        &ready_file,
        &stdout,
        &stderr,
    );
    match outcome {
        Ok(info) => Ok(info),
        Err(error) => {
            reap_launched_session(workspace);
            Err(error)
        }
    }
}
