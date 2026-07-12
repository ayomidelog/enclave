mod idmap;
mod process;
mod script;
mod security;
mod userns;

use std::fs;
use std::fs::OpenOptions;
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::types::WorkspaceMetadata;

pub(crate) use idmap::workspace_bind_mount_idmap_option;
pub use process::{
    count_processes_in_pid_namespace, process_alive, process_matches, process_resource_usage,
    process_starttime_ticks, read_namespace_refs,
};
pub(crate) use script::WORKSPACE_SESSION_SCRIPT;
pub(crate) use security::{
    apply_exec_restrictions, apply_session_restrictions, detach_old_root, mask_runtime_paths,
    tighten_namespace_mounts,
};
pub(crate) use userns::{detect_user_namespace_mode, UserNamespaceMode};

const START_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(3);
const SESSION_HELPER_BASENAME: &str = "session-helper";
const SELF_EXE_PATH: &str = "/proc/self/exe";
const HELPER_OVERRIDE_ENV: &str = "ENCLAVE_SELF_EXE";
const TEST_BINARY_ENV: &str = "CARGO_BIN_EXE_enclave";

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub pid: u32,
    pub starttime_ticks: u64,
    pub mount_ns: String,
    pub pid_ns: String,
}

#[derive(Debug, Default)]
pub struct BatchStopResult {
    pub stopped_pids: BTreeSet<u32>,
    pub failed_pids: BTreeSet<u32>,
}

pub fn start_session(
    workspace: &WorkspaceMetadata,
    apparmor_profile: Option<&str>,
    selinux_label: Option<&str>,
) -> Result<SessionInfo> {
    ensure_runtime_layout(workspace)?;
    let userns = detect_user_namespace_mode()?;
    let current_exe = prepare_session_helper(workspace)?;
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

    let status = Command::new("setsid")
        .arg("-f")
        .arg(&current_exe)
        .arg("internal")
        .arg("workspace-session-launch")
        .args(launch_userns_args(&userns))
        .arg("--rootfs")
        .arg(&workspace.sandbox_rootfs_path)
        .arg("--workspace-fs")
        .arg(workspace_source)
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
        .arg(workspace_bind_idmap.unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status()
        .context("failed to launch workspace session via setsid/unshare")?;

    if !status.success() {
        bail!("failed to launch workspace session (status {status})");
    }

    let started = Instant::now();
    while started.elapsed() < START_TIMEOUT {
        if ready_file.exists() {
            let pid = match process::read_pid_file(&pid_file) {
                Ok(pid) => pid,
                Err(_) if !pid_file.exists() => {
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
                Err(other_err) => return Err(other_err),
            };
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
            return Ok(SessionInfo {
                pid,
                starttime_ticks,
                mount_ns,
                pid_ns,
            });
        }
        thread::sleep(Duration::from_millis(50));
    }

    let tail = process::read_log_tail(&log_file, 20).unwrap_or_default();
    let rendered_tail = if tail.trim().is_empty() {
        "<empty>".to_string()
    } else {
        tail
    };
    bail!(
        "workspace session did not become ready within {}s (expected files: {}, {}). log file: {}. recent log:\n{}",
        START_TIMEOUT.as_secs(),
        pid_file.display(),
        ready_file.display(),
        log_file.display(),
        rendered_tail
    )
}

fn session_helper_path(workspace: &WorkspaceMetadata) -> PathBuf {
    runtime_dir(workspace).join(SESSION_HELPER_BASENAME)
}

fn prepare_session_helper(workspace: &WorkspaceMetadata) -> Result<PathBuf> {
    let helper_path = session_helper_path(workspace);
    let temp_path = runtime_dir(workspace).join(format!("{SESSION_HELPER_BASENAME}.tmp"));
    let source_exe = resolve_session_helper_source();
    if temp_path.exists() {
        fs::remove_file(&temp_path)
            .with_context(|| format!("failed to remove stale {}", temp_path.display()))?;
    }
    fs::copy(&source_exe, &temp_path).with_context(|| {
        format!(
            "failed to copy session helper from {} to {}",
            source_exe.display(),
            temp_path.display()
        )
    })?;
    let mut permissions = fs::metadata(&temp_path)
        .with_context(|| format!("failed to stat {}", temp_path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&temp_path, permissions)
        .with_context(|| format!("failed to chmod {}", temp_path.display()))?;
    fs::rename(&temp_path, &helper_path).with_context(|| {
        format!(
            "failed to promote session helper {} -> {}",
            temp_path.display(),
            helper_path.display()
        )
    })?;
    Ok(helper_path)
}

pub(crate) fn resolve_session_helper_source() -> PathBuf {
    for candidate in [HELPER_OVERRIDE_ENV, TEST_BINARY_ENV] {
        if let Some(path) = std::env::var_os(candidate).map(PathBuf::from) {
            if path.is_file() {
                return path;
            }
        }
    }
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(inferred) = infer_workspace_helper_from_current_exe(&current_exe) {
            return inferred;
        }
    }
    PathBuf::from(SELF_EXE_PATH)
}

fn infer_workspace_helper_from_current_exe(current_exe: &Path) -> Option<PathBuf> {
    let deps_dir = current_exe.parent()?;
    if deps_dir.file_name()? != "deps" {
        return None;
    }
    let profile_dir = deps_dir.parent()?;
    let candidate = profile_dir.join("enclave");
    candidate.is_file().then_some(candidate)
}

fn setgroups_args(userns: &userns::UserNamespacePlan) -> Vec<&'static str> {
    if userns.gid_map.count > 1 {
        vec!["--deny-setgroups"]
    } else {
        Vec::new()
    }
}

fn launch_userns_args(userns: &UserNamespaceMode) -> Vec<String> {
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

pub fn stop_session(pid: u32, expected_starttime_ticks: Option<u64>) -> Result<()> {
    let result = stop_sessions_batch(&[(pid, expected_starttime_ticks)])?;
    if result.failed_pids.contains(&pid) {
        bail!("workspace session pid {} did not exit after SIGKILL", pid);
    }
    Ok(())
}

pub fn stop_sessions_batch(targets: &[(u32, Option<u64>)]) -> Result<BatchStopResult> {
    let mut result = BatchStopResult::default();
    let mut pending = Vec::new();

    for (pid, expected_starttime_ticks) in targets.iter().copied() {
        if !process_matches(pid, expected_starttime_ticks) {
            result.stopped_pids.insert(pid);
            continue;
        }
        match process::verify_signal_target(pid, expected_starttime_ticks) {
            Ok(()) => pending.push((pid, expected_starttime_ticks)),
            Err(err) => {
                let msg = format!("{err:#}");
                if msg.contains("refusing to signal") || msg.contains("does not look like") {
                    tracing::warn!(
                        "stale session pid {} detected (not an enclave process); treating as already stopped",
                        pid
                    );
                    result.stopped_pids.insert(pid);
                    continue;
                }
                return Err(err);
            }
        }
    }

    for (pid, _) in &pending {
        process::send_signal(*pid, libc::SIGTERM)?;
    }
    wait_for_targets_to_exit(&pending, STOP_TIMEOUT);

    let mut remaining = collect_running_targets(&pending);
    for (pid, _) in &remaining {
        process::send_signal(*pid, libc::SIGKILL)?;
    }
    wait_for_targets_to_exit(&remaining, Duration::from_secs(1));

    for (pid, expected_starttime_ticks) in pending {
        if process_matches(pid, expected_starttime_ticks) {
            result.failed_pids.insert(pid);
        } else {
            result.stopped_pids.insert(pid);
        }
    }

    remaining.clear();
    Ok(result)
}

fn wait_for_targets_to_exit(targets: &[(u32, Option<u64>)], timeout: Duration) {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if targets
            .iter()
            .all(|(pid, expected_starttime_ticks)| !process_matches(*pid, *expected_starttime_ticks))
        {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn collect_running_targets(targets: &[(u32, Option<u64>)]) -> Vec<(u32, Option<u64>)> {
    targets
        .iter()
        .copied()
        .filter(|(pid, expected_starttime_ticks)| process_matches(*pid, *expected_starttime_ticks))
        .collect()
}

pub fn runtime_pid_file(workspace: &WorkspaceMetadata) -> PathBuf {
    runtime_dir(workspace).join("session.pid")
}

pub fn runtime_ready_file(workspace: &WorkspaceMetadata) -> PathBuf {
    runtime_dir(workspace).join("session.ready")
}

pub fn namespace_ref_paths(workspace: &WorkspaceMetadata) -> (PathBuf, PathBuf) {
    (
        resolve_namespace_ref_path(workspace, &workspace.namespace_refs.mount, "mnt.ref"),
        resolve_namespace_ref_path(workspace, &workspace.namespace_refs.pid, "pid.ref"),
    )
}

pub fn write_namespace_ref_values(
    workspace: &WorkspaceMetadata,
    mount_value: &str,
    pid_value: &str,
) -> Result<()> {
    let (mount_ref_path, pid_ref_path) = namespace_ref_paths(workspace);
    if let Some(parent) = mount_ref_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if let Some(parent) = pid_ref_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    crate::fsutil::write_file_atomic(
        &mount_ref_path,
        format!("{mount_value}\n").as_bytes(),
        0o600,
    )
    .with_context(|| format!("failed to write {}", mount_ref_path.display()))?;
    crate::fsutil::write_file_atomic(&pid_ref_path, format!("{pid_value}\n").as_bytes(), 0o600)
        .with_context(|| format!("failed to write {}", pid_ref_path.display()))?;
    Ok(())
}

pub fn runtime_log_file(workspace: &WorkspaceMetadata) -> PathBuf {
    runtime_dir(workspace).join("session.log")
}

fn runtime_dir(workspace: &WorkspaceMetadata) -> PathBuf {
    PathBuf::from(&workspace.workspace_path).join("runtime")
}

fn ensure_runtime_layout(workspace: &WorkspaceMetadata) -> Result<()> {
    for path in [
        &workspace.workspace_path,
        &workspace.filesystem_path,
        &workspace.overlay_home_upper_path,
        &workspace.overlay_home_work_path,
        &workspace.overlay_home_merged_path,
    ] {
        fs::create_dir_all(path).with_context(|| format!("failed to create {}", path))?;
    }

    let (mount_ref_path, pid_ref_path) = namespace_ref_paths(workspace);
    if let Some(parent) = mount_ref_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if let Some(parent) = pid_ref_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::create_dir_all(runtime_dir(workspace))
        .with_context(|| format!("failed to create runtime dir for {}", workspace.id))?;
    if workspace.home_mount_source_path.is_some() {
        return Ok(());
    }
    super::create::ensure_traversable_directory_permissions(Path::new(&workspace.filesystem_path))?;
    Ok(())
}

fn resolve_namespace_ref_path(
    workspace: &WorkspaceMetadata,
    configured: &str,
    default_name: &str,
) -> PathBuf {
    let default_path = PathBuf::from(&workspace.workspace_path)
        .join("ns")
        .join(default_name);
    if configured.is_empty() || configured == "unassigned" {
        return default_path;
    }

    default_path
}

#[cfg(test)]
#[path = "../../../tests/src/workspace/session/mod.rs"]
mod tests;
