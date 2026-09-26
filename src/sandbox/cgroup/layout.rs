//! The cgroup directory tree: finding it, creating it, and removing it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::config::{apply_cgroup_limits, CgroupConfig};

pub(super) const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const MANAGED_CONTROLLERS: &[&str] = &["memory", "cpu", "pids"];

/// Whether the host exposes the cgroup v2 unified hierarchy.
///
/// The answer is a property of the running kernel and its mount layout, so it
/// cannot change while the daemon runs, and the check is on every quota-backed
/// workspace start. Answering it once keeps that start path from paying a
/// syscall per cgroup operation and keeps the answer consistent between them.
pub fn is_cgroup_v2_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| Path::new(CGROUP_ROOT).join("cgroup.controllers").exists())
}

pub fn available_controllers() -> Vec<String> {
    let path = PathBuf::from(CGROUP_ROOT).join("cgroup.controllers");
    match fs::read_to_string(&path) {
        Ok(raw) => raw.split_whitespace().map(String::from).collect(),
        Err(err) => {
            tracing::warn!(
                "failed to read cgroup controllers from {}: {}",
                path.display(),
                err
            );
            Vec::new()
        }
    }
}

pub fn sandbox_cgroup_name(sandbox_id: &str) -> String {
    format!("enclave-sb-{sandbox_id}")
}

pub fn ensure_sandbox_cgroup(
    name: &str,
    config: &CgroupConfig,
    create_if_empty: bool,
) -> Result<Option<PathBuf>> {
    ensure_child_cgroup(
        &PathBuf::from(CGROUP_ROOT),
        name,
        config,
        create_if_empty,
        true,
    )
}

pub fn create_workspace_cgroup(name: &str, config: &CgroupConfig) -> Result<Option<PathBuf>> {
    ensure_child_cgroup(
        &PathBuf::from(CGROUP_ROOT),
        name,
        config,
        config.has_limits(),
        false,
    )
}

pub fn ensure_workspace_cgroup(
    parent: &Path,
    name: &str,
    config: &CgroupConfig,
    create_if_empty: bool,
) -> Result<Option<PathBuf>> {
    ensure_child_cgroup(parent, name, config, create_if_empty, false)
}

/// The cgroup a process is currently a member of, read from its own view.
pub fn runtime_cgroup_path(pid: u32) -> Result<Option<PathBuf>> {
    let raw = fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .with_context(|| format!("failed to read /proc/{pid}/cgroup"))?;
    for line in raw.lines() {
        let mut parts = line.splitn(3, ':');
        let hierarchy = parts.next().unwrap_or_default();
        let controllers = parts.next().unwrap_or_default();
        let relative_path = parts.next().unwrap_or_default();
        if hierarchy != "0" || !controllers.is_empty() {
            continue;
        }
        let trimmed = relative_path.trim_start_matches('/');
        if trimmed.is_empty() {
            return Ok(Some(PathBuf::from(CGROUP_ROOT)));
        }
        return Ok(Some(PathBuf::from(CGROUP_ROOT).join(trimmed)));
    }
    Ok(None)
}

pub fn remove_workspace_cgroup(name: &str) -> Result<()> {
    validate_cgroup_name(name)?;
    remove_cgroup_path(&PathBuf::from(CGROUP_ROOT).join(name))
}

pub fn remove_cgroup_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    // A cgroup that is still draining cannot be removed for a moment after its
    // last process leaves, so removal is retried a bounded number of times with
    // backoff. The kernel reports that state two different ways and both are
    // retryable: `EBUSY` while a process is still listed, and `ENOTEMPTY` while a
    // child cgroup is. Each wait is recorded, because a retry that costs a second
    // is the difference between a fast stop and a slow one.
    //
    // The window this covers is real rather than theoretical. A command a
    // workspace ran is reaped by the kernel when the runtime that owned its pid
    // namespace exits, and the helper that launched it is itself a process in the
    // workspace cgroup that only exits once its own wait returns, a moment later.
    // Retrying only `ENOTEMPTY` left that moment as a reported cleanup failure on
    // a stop that had actually released everything.
    let mut last_error = None;
    for attempt in 0..CGROUP_REMOVE_ATTEMPTS {
        match fs::remove_dir(path) {
            Ok(()) => {
                if path.exists() {
                    bail!("cgroup {} remained after removal", path.display());
                }
                return Ok(());
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) if cgroup_removal_is_retryable(&err) => {
                last_error = Some(err);
                if attempt + 1 < CGROUP_REMOVE_ATTEMPTS {
                    let delay = Duration::from_millis(10 * (u64::from(attempt) + 1));
                    crate::perf::record_cleanup_retry();
                    crate::perf::record_cleanup_retry_delay(delay.as_micros() as u64);
                    thread::sleep(delay);
                }
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to remove cgroup {}", path.display()));
            }
        }
    }

    // The attempts are spent, so this is the retry that did not recover. The errno
    // is the kernel's own answer for why the last attempt failed, which is what tells
    // an operator whether the cgroup was still busy or had already gone.
    crate::perf::record_cleanup_retry_exhausted(
        last_error.as_ref().and_then(|err| err.raw_os_error()),
    );
    match last_error {
        Some(err) => Err(err).with_context(|| {
            format!(
                "cgroup {} is still busy after {} attempts; processes or child cgroups remain",
                path.display(),
                CGROUP_REMOVE_ATTEMPTS
            )
        }),
        None => bail!("cgroup {} could not be removed", path.display()),
    }
}

/// How many times a cgroup removal is retried while the cgroup is draining.
///
/// The backoff doubles from ten milliseconds, so the whole schedule is a bounded
/// 150 ms that only a genuinely busy cgroup pays.
const CGROUP_REMOVE_ATTEMPTS: u32 = 5;

/// Whether the kernel's refusal to remove a cgroup is a state that clears itself.
fn cgroup_removal_is_retryable(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::DirectoryNotEmpty
        || error.raw_os_error() == Some(libc::EBUSY)
}

fn ensure_child_cgroup(
    parent: &Path,
    name: &str,
    config: &CgroupConfig,
    create_if_empty: bool,
    enable_children: bool,
) -> Result<Option<PathBuf>> {
    if !is_cgroup_v2_available() {
        return Ok(None);
    }
    if !config.has_limits() && !create_if_empty {
        return Ok(None);
    }

    validate_cgroup_name(name)?;
    enable_managed_controllers(&parent.join("cgroup.subtree_control"));

    let cgroup_path = parent.join(name);
    fs::create_dir_all(&cgroup_path)
        .with_context(|| format!("failed to create cgroup {}", cgroup_path.display()))?;
    apply_cgroup_limits(&cgroup_path, config)?;
    if enable_children {
        enable_managed_controllers(&cgroup_path.join("cgroup.subtree_control"));
    }

    Ok(Some(cgroup_path))
}

fn enable_managed_controllers(subtree_control: &Path) {
    for controller in MANAGED_CONTROLLERS {
        if let Err(err) = super::write_cgroup_value(subtree_control, &format!("+{controller}")) {
            tracing::warn!("failed to enable {} controller: {err:#}", controller);
        }
    }
}

pub(crate) fn validate_cgroup_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("cgroup name must not be empty");
    }
    if name.starts_with('/') {
        bail!("cgroup name must not be absolute: {name}");
    }
    if name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        bail!("cgroup name contains unsafe path components: {name}");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        bail!("cgroup name contains disallowed characters: {name}");
    }
    Ok(())
}
