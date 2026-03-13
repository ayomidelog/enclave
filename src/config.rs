use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileConfig {
    pub socket: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub pid_file: Option<PathBuf>,
    pub debootstrap_binary: Option<String>,
    pub workspace_apparmor_profile: Option<String>,
    pub workspace_selinux_label: Option<String>,
    pub suite: Option<String>,
    pub mirror: Option<String>,
    pub wait_secs: Option<u64>,
    pub bootstrap_method: Option<String>,
}

pub fn load_config(explicit_path: Option<&Path>) -> Result<FileConfig> {
    let Some(path) = resolve_config_path(explicit_path) else {
        return Ok(FileConfig::default());
    };

    if !path.exists() {
        if explicit_path.is_some() {
            anyhow::bail!(
                "config file not found at {} (specified via --config)",
                path.display()
            );
        }
        return Ok(FileConfig::default());
    }

    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    let parsed: FileConfig = toml::from_str(&raw)
        .with_context(|| format!("failed to parse config {}", path.display()))?;
    validate_file_config(&parsed).with_context(|| {
        format!(
            "invalid semantic values in config {}; fix debootstrap_binary/suite/mirror",
            path.display()
        )
    })?;
    Ok(parsed)
}

fn validate_file_config(config: &FileConfig) -> Result<()> {
    for (label, value) in [
        (
            "workspace_apparmor_profile",
            config.workspace_apparmor_profile.as_deref(),
        ),
        (
            "workspace_selinux_label",
            config.workspace_selinux_label.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            if value.trim().is_empty() || value.chars().any(char::is_control) {
                anyhow::bail!("{label} must be a non-empty label without control characters");
            }
        }
    }

    if let Some(binary) = config.debootstrap_binary.as_deref() {
        crate::sandbox::validate_debootstrap_binary(binary)?;
    }

    let suite = config
        .suite
        .as_deref()
        .unwrap_or(crate::sandbox::DEFAULT_DEBIAN_SUITE);
    let mirror = config
        .mirror
        .as_deref()
        .unwrap_or(crate::sandbox::DEFAULT_DEBIAN_MIRROR);
    crate::sandbox::validate_debootstrap_inputs(suite, mirror)?;
    Ok(())
}

fn resolve_config_path(explicit_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit_path {
        return Some(path.to_path_buf());
    }
    if let Ok(config_home) = env::var("XDG_CONFIG_HOME") {
        return Some(
            PathBuf::from(config_home)
                .join("enclave")
                .join("config.toml"),
        );
    }
    if let Ok(home) = env::var("HOME") {
        return Some(
            PathBuf::from(home)
                .join(".config")
                .join("enclave")
                .join("config.toml"),
        );
    }
    None
}

#[cfg(test)]
#[path = "../tests/src/config.rs"]
mod tests;
