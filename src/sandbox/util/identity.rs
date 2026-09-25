//! Naming a sandbox, resolving a selector, and checking the bootstrap
//! program it will run.
//!
//! A name is what an operator types and an id is what the state directory
//! uses, so a selector may be either. An ambiguous name is refused rather
//! than guessed. The bootstrap program is checked here because a missing or
//! unexecutable one fails a sandbox creation minutes later, after the rootfs
//! has already been copied.

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use uuid::Uuid;

use crate::registry::Registry;

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
        0 => Err(crate::error::coded(
            crate::error::ErrorCode::NotFound,
            format!("sandbox '{}' not found (by id or name)", selector),
        )),
        1 => Ok(matches.remove(0)),
        _ => Err(crate::error::coded(
            crate::error::ErrorCode::Conflict,
            format!(
                "sandbox name '{}' is ambiguous; use id instead (matches: {})",
                selector,
                matches.join(", ")
            ),
        )),
    }
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

// Used by the identity tests.
#[cfg(test)]
#[path = "../../../tests/src/sandbox/util.rs"]
mod tests;
