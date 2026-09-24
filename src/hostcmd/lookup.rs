//! Finding an executable and classifying a command failure.
//!
//! These answer capability questions from the filesystem instead of by starting a
//! shell or a probe process, because they run on every daemon start and every
//! quota-backed workspace start.

use std::time::Duration;

use super::{HostCommandError, DEFAULT_TIMEOUT, MAX_TIMEOUT};

pub(crate) fn configured_timeout() -> Duration {
    match std::env::var("ENCLAVE_HOST_COMMAND_TIMEOUT_SECS") {
        Ok(value) => match value.parse::<u64>() {
            Ok(seconds) if seconds > 0 => Duration::from_secs(seconds).min(MAX_TIMEOUT),
            _ => {
                tracing::warn!(
                    "ignoring ENCLAVE_HOST_COMMAND_TIMEOUT_SECS value {value}; using {:?}",
                    DEFAULT_TIMEOUT
                );
                DEFAULT_TIMEOUT
            }
        },
        Err(_) => DEFAULT_TIMEOUT,
    }
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
