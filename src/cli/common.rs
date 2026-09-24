use clap::Args;

use crate::resource_limits::validate_cpu_percent;
use crate::sandbox::{BootstrapMethod, DEFAULT_DEBIAN_MIRROR, DEFAULT_DEBIAN_SUITE};

#[derive(Args, Debug)]
pub struct AuthProviderArgs {
    #[arg(value_parser = parse_entity_name)]
    pub provider: String,
}

#[derive(Args, Debug)]
pub struct DoctorArgs {
    #[arg(long)]
    pub repair: bool,
}

#[derive(Args, Debug)]
pub struct UpArgs {
    #[arg(long)]
    pub rebuild: bool,
    #[arg(long)]
    pub cache_setup: bool,
}

#[derive(Args, Debug)]
pub struct RestartArgs {
    #[arg(long)]
    pub rebuild: bool,
    #[arg(long)]
    pub cache_setup: bool,
}

#[derive(Args, Debug)]
pub struct CreateArgs {
    #[arg(value_parser = parse_entity_name)]
    pub name: String,
    #[arg(long, default_value = DEFAULT_DEBIAN_SUITE)]
    pub suite: String,
    #[arg(long, default_value = DEFAULT_DEBIAN_MIRROR)]
    pub mirror: String,
    #[arg(long, default_value = "debootstrap", value_parser = parse_bootstrap_method)]
    pub bootstrap_method: BootstrapMethod,
    #[arg(long)]
    pub memory_mb: Option<u64>,
    #[arg(long, value_parser = parse_cpu_percent_arg)]
    pub cpu_percent: Option<f64>,
    #[arg(long)]
    pub max_procs: Option<u64>,
}

pub(super) fn parse_bootstrap_method(s: &str) -> Result<BootstrapMethod, String> {
    s.parse::<BootstrapMethod>().map_err(|e| e.to_string())
}

pub(super) fn parse_cpu_percent_arg(s: &str) -> Result<f64, String> {
    let value = s
        .parse::<f64>()
        .map_err(|_| "cpu_percent must be a number".to_string())?;
    validate_cpu_percent(value).map_err(|err| err.to_string())?;
    Ok(value)
}

pub(super) const MAX_ENTITY_NAME_LEN: usize = 63;

pub(super) fn parse_non_empty_arg(s: &str) -> Result<String, String> {
    if s.is_empty() {
        return Err("value must not be empty".to_string());
    }
    if s.chars().any(char::is_control) {
        return Err("value must not contain control characters".to_string());
    }
    Ok(s.to_string())
}

pub(super) fn parse_entity_name(s: &str) -> Result<String, String> {
    if s.is_empty() || s.len() > MAX_ENTITY_NAME_LEN {
        return Err(format!("value must be 1-{MAX_ENTITY_NAME_LEN} characters"));
    }
    if s.chars().any(char::is_control) {
        return Err("value must not contain control characters".to_string());
    }
    let mut chars = s.chars();
    let first = chars
        .next()
        .ok_or_else(|| "value is required".to_string())?;
    if !first.is_ascii_alphanumeric() {
        return Err("value must start with an ASCII letter or digit".to_string());
    }
    if chars.any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
        return Err("value may only contain ASCII letters, digits, '-' and '_'".to_string());
    }
    Ok(s.to_string())
}

pub(super) fn parse_shell_path(s: &str) -> Result<String, String> {
    if s.trim().is_empty() {
        return Err("shell path must not be empty".to_string());
    }
    if s.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("shell must be an absolute path without spaces or arguments".to_string());
    }
    if !s.starts_with('/') {
        return Err("shell must be an absolute path".to_string());
    }
    Ok(s.to_string())
}

#[derive(Args, Debug)]
pub struct PsArgs {
    #[arg(long, visible_alias = "project")]
    pub local: bool,
}
