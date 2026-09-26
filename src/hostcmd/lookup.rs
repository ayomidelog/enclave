//! Finding an executable and classifying a command failure.
//!
//! These answer capability questions from the filesystem instead of by starting a
//! shell or a probe process, because they run on every daemon start and every
//! quota-backed workspace start.

use std::time::Duration;

use super::HostCommandError;

/// The deadline for a command that does not set its own.
///
/// The value and its clamp live in the deadline table with every other lifecycle
/// bound, so an operator reads and sets all of them the same way and daemon health
/// reports the whole set.
pub(crate) fn configured_timeout() -> Duration {
    crate::deadlines::host_command().get()
}

/// Resolve a program the way a shell would, without starting one.
///
/// Capability probes run on every daemon start and every quota-backed workspace
/// start. Answering them from the filesystem avoids a fork per probe and keeps
/// the answer stable for the life of the process.
pub(crate) fn command_on_path(program: &str) -> Option<std::path::PathBuf> {
    let candidate = std::path::Path::new(program);
    if candidate.is_absolute() {
        return is_executable(candidate).then(|| candidate.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        let candidate = directory.join(program);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// True when the error is a host-command deadline that expired.
pub(crate) fn is_timeout(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<HostCommandError>()
        .is_some_and(HostCommandError::is_timeout)
}
