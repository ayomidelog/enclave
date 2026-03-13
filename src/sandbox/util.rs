use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, bail, Context, Result};
use uuid::Uuid;

use crate::registry::Registry;

use super::types::SandboxMetadata;

pub(crate) fn sandboxes_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("sandboxes")
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 63 {
        bail!("sandbox name must be 1-63 characters");
    }

    let mut chars = name.chars();
    let first = chars
        .next()
        .ok_or_else(|| anyhow!("invalid sandbox name"))?;
    if !first.is_ascii_alphanumeric() {
        bail!("sandbox name must start with an ASCII letter or digit");
    }

    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            bail!("sandbox name contains invalid character '{}'", c);
        }
    }

    Ok(())
}

pub(crate) fn generate_sandbox_id(name: &str) -> String {
    let slug = crate::fsutil::slugify(name, "sandbox");
    let random = Uuid::new_v4().simple().to_string();
    format!("{slug}-{}", &random[..12])
}

pub(crate) fn run_command_with_live_log(
    command: &mut Command,
    log_path: &Path,
    label: &str,
) -> Result<Output> {
    let log_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(log_path)
        .with_context(|| format!("failed to open {} log {}", label, log_path.display()))?;
    let log_writer = Arc::new(Mutex::new(log_file));
    {
        let mut log = log_writer
            .lock()
            .map_err(|_| anyhow!("failed to lock {} log writer", label))?;
        writeln!(log, "# {} live log", label)?;
        writeln!(log)?;
    }

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {}", label))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture {} stdout", label))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture {} stderr", label))?;

    let stdout_handle = pump_command_stream(stdout, "stdout", Arc::clone(&log_writer));
    let stderr_handle = pump_command_stream(stderr, "stderr", Arc::clone(&log_writer));

    let status = child
        .wait()
        .with_context(|| format!("failed waiting for {}", label))?;
    let stdout = join_stream_capture(stdout_handle, label, "stdout")?;
    let stderr = join_stream_capture(stderr_handle, label, "stderr")?;

    {
        let mut log = log_writer
            .lock()
            .map_err(|_| anyhow!("failed to lock {} log writer", label))?;
        writeln!(log)?;
        writeln!(log, "# {} exit status: {}", label, status)?;
        log.flush()?;
    }

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn pump_command_stream<R>(
    reader: R,
    stream_label: &'static str,
    log_writer: Arc<Mutex<fs::File>>,
) -> thread::JoinHandle<Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut captured = Vec::new();

        loop {
            let mut line = Vec::new();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }

            captured.extend_from_slice(&line);
            let mut log = log_writer
                .lock()
                .map_err(|_| anyhow!("failed to lock command log writer"))?;
            log.write_all(format!("[{}] ", stream_label).as_bytes())?;
            log.write_all(&line)?;
            if !line.ends_with(b"\n") {
                log.write_all(b"\n")?;
            }
            log.flush()?;
        }

        Ok(captured)
    })
}

fn join_stream_capture(
    handle: thread::JoinHandle<Result<Vec<u8>>>,
    label: &str,
    stream_label: &str,
) -> Result<Vec<u8>> {
    match handle.join() {
        Ok(result) => {
            result.with_context(|| format!("failed to capture {} {}", label, stream_label))
        }
        Err(_) => bail!("{} {} stream thread panicked", label, stream_label),
    }
}

pub(crate) fn command_failure_detail(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return stdout;
    }

    "debootstrap did not return stderr/stdout".to_string()
}

pub fn normalize_sandbox_metadata(metadata: &mut SandboxMetadata) {
    let sandbox_dir = PathBuf::from(&metadata.sandbox_path);
    if metadata.mounted_rootfs_path.is_empty() {
        metadata.mounted_rootfs_path = sandbox_dir
            .join("runtime")
            .join("rootfs.mnt")
            .to_string_lossy()
            .to_string();
    }
    if metadata.workspaces_path.is_empty() {
        metadata.workspaces_path = sandbox_dir.join("workspaces").to_string_lossy().to_string();
    }
    if metadata.home_base_path.is_empty() {
        metadata.home_base_path = sandbox_dir.join("home-base").to_string_lossy().to_string();
    }
}

pub fn ensure_sandbox_layout(metadata: &SandboxMetadata) -> Result<()> {
    fs::create_dir_all(&metadata.mounted_rootfs_path)
        .with_context(|| format!("failed to create {}", metadata.mounted_rootfs_path))?;
    fs::create_dir_all(&metadata.workspaces_path)
        .with_context(|| format!("failed to create {}", metadata.workspaces_path))?;
    fs::create_dir_all(&metadata.home_base_path)
        .with_context(|| format!("failed to create {}", metadata.home_base_path))?;
    Ok(())
}

pub fn effective_rootfs_path(metadata: &SandboxMetadata) -> String {
    if metadata.status == super::types::SandboxStatus::Running
        && !metadata.mounted_rootfs_path.is_empty()
    {
        return metadata.mounted_rootfs_path.clone();
    }
    metadata.rootfs_path.clone()
}

pub(crate) fn validate_debootstrap_inputs(suite: &str, mirror: &str) -> Result<()> {
    const ALLOWED_SUITES: &[&str] = &[
        "bookworm",
        "bullseye",
        "trixie",
        "sid",
        "stable",
        "testing",
        "oldstable",
    ];

    if suite.is_empty() || suite.len() > 32 {
        bail!("suite must be 1-32 characters");
    }
    if !suite
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!("suite contains invalid characters");
    }
    if suite.starts_with('-') {
        bail!("suite must not start with '-'");
    }
    if !ALLOWED_SUITES.contains(&suite) {
        bail!(
            "unsupported suite '{}'; allowed values: {}",
            suite,
            ALLOWED_SUITES.join(", ")
        );
    }

    if mirror.is_empty() || mirror.len() > 200 {
        bail!("mirror must be 1-200 characters");
    }
    if mirror.chars().any(|c| c.is_whitespace()) {
        bail!("mirror must not contain whitespace");
    }
    if !(mirror.starts_with("http://") || mirror.starts_with("https://")) {
        bail!("mirror must start with http:// or https://");
    }
    if mirror.starts_with("file://") {
        bail!("file:// mirrors are not allowed");
    }
    let host_part = mirror
        .strip_prefix("http://")
        .or_else(|| mirror.strip_prefix("https://"))
        .unwrap_or_default();
    let host = host_part.split('/').next().unwrap_or_default();
    if host.is_empty() || host.ends_with(':') {
        bail!("mirror URL must include a host");
    }

    Ok(())
}

pub(crate) fn validate_debootstrap_binary(binary: &str) -> Result<PathBuf> {
    if binary.trim().is_empty() {
        bail!("debootstrap binary must not be empty");
    }
    if binary.contains('/') {
        let path = PathBuf::from(binary);
        validate_executable_file(&path, "debootstrap binary")?;
        return Ok(path);
    }

    let path_env = env::var_os("PATH").ok_or_else(|| anyhow!("PATH is not set"))?;
    for dir in env::split_paths(&path_env) {
        let candidate = dir.join(binary);
        if !candidate.exists() {
            continue;
        }
        validate_executable_file(&candidate, "debootstrap binary")?;
        return Ok(candidate);
    }

    bail!("failed to resolve debootstrap binary '{}' in PATH", binary)
}

pub fn resolve_sandbox_id(registry: &Registry, selector: &str) -> Result<String> {
    if registry.sandboxes.contains_key(selector) {
        return Ok(selector.to_string());
    }

    let mut matches = Vec::new();
    for (id, sandbox) in &registry.sandboxes {
        if sandbox.metadata.name == selector {
            matches.push(id.clone());
        }
    }

    match matches.len() {
        0 => bail!("sandbox '{}' not found (by id or name)", selector),
        1 => Ok(matches.remove(0)),
        _ => bail!(
            "sandbox name '{}' is ambiguous; use id instead (matches: {})",
            selector,
            matches.join(", ")
        ),
    }
}

pub(crate) fn dir_size(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }

    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];

    while let Some(current) = stack.pop() {
        let metadata = fs::symlink_metadata(&current)
            .with_context(|| format!("failed to stat {}", current.display()))?;

        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
            continue;
        }

        if metadata.is_dir() {
            for entry in fs::read_dir(&current)
                .with_context(|| format!("failed to read {}", current.display()))?
            {
                stack.push(entry?.path());
            }
        }
    }

    Ok(total)
}

fn validate_executable_file(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to stat {} {}", label, path.display()))?;
    if !metadata.is_file() {
        bail!("{} {} is not a regular file", label, path.display());
    }
    let mode = metadata.permissions().mode();
    if mode & 0o111 == 0 {
        bail!("{} {} is not executable", label, path.display());
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/src/sandbox/util.rs"]
mod tests;
