use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::resource_limits::validate_cpu_percent;
use crate::sandbox::BootstrapMethod;

pub const ENCLAVEFILE_NAME: &str = "Enclavefile";

#[derive(Debug, Deserialize)]
pub struct Enclavefile {
    pub sandbox: SandboxSection,
    #[serde(default)]
    pub workspace: BTreeMap<String, WorkspaceSection>,
}

#[derive(Debug, Deserialize)]
pub struct SandboxSection {
    pub name: String,
    #[serde(default = "default_suite")]
    pub suite: String,
    #[serde(default)]
    pub bootstrap_method: BootstrapMethod,
    pub memory_mb: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub max_procs: Option<u64>,
    #[serde(default)]
    pub setup: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceSection {
    pub name: String,
    pub run: Option<String>,
    pub path: Option<String>,
    pub workspace_dir: Option<String>,
    pub cpu_seconds: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub memory_mb: Option<u64>,
    pub max_procs: Option<u64>,
    pub max_open_files: Option<u64>,
    pub disk_mb: Option<u64>,
    #[serde(default)]
    pub auth: Vec<String>,
    #[serde(default)]
    pub env_tokens: Vec<String>,
    #[serde(default)]
    pub ports: Vec<String>,
}

fn default_suite() -> String {
    "bookworm".to_string()
}

pub fn find_enclavefile(start_dir: &Path) -> Option<PathBuf> {
    let candidate = start_dir.join(ENCLAVEFILE_NAME);
    if candidate.is_file() {
        return Some(candidate);
    }
    None
}

pub fn load_enclavefile(path: &Path) -> Result<Enclavefile> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    parse_enclavefile(&content)
}

pub fn parse_enclavefile(content: &str) -> Result<Enclavefile> {
    let enclavefile: Enclavefile =
        toml::from_str(content).context("failed to parse Enclavefile")?;
    validate_enclavefile(&enclavefile)?;
    Ok(enclavefile)
}

fn validate_enclavefile(enclavefile: &Enclavefile) -> Result<()> {
    if enclavefile.sandbox.name.is_empty() {
        bail!("Enclavefile: sandbox.name must not be empty");
    }
    if enclavefile.sandbox.suite.is_empty() {
        bail!("Enclavefile: sandbox.suite must not be empty");
    }
    if let Some(cpu_percent) = enclavefile.sandbox.cpu_percent {
        validate_cpu_percent(cpu_percent)
            .map_err(|err| anyhow::anyhow!("Enclavefile: sandbox.cpu_percent is invalid: {err}"))?;
    }
    for (key, ws) in &enclavefile.workspace {
        if ws.name.is_empty() {
            bail!("Enclavefile: workspace.{}.name must not be empty", key);
        }
        if let Some(cpu_percent) = ws.cpu_percent {
            validate_cpu_percent(cpu_percent).map_err(|err| {
                anyhow::anyhow!(
                    "Enclavefile: workspace.{}.cpu_percent is invalid: {}",
                    key,
                    err
                )
            })?;
        }
        if let Some(path) = &ws.path {
            if path.trim().is_empty() {
                bail!("Enclavefile: workspace.{}.path must not be empty", key);
            }
        }
        if let Some(path) = &ws.workspace_dir {
            if path.trim().is_empty() {
                bail!(
                    "Enclavefile: workspace.{}.workspace_dir must not be empty",
                    key
                );
            }
        }
        if ws.path.is_some() && ws.workspace_dir.is_some() {
            bail!(
                "Enclavefile: workspace.{} cannot define both path and workspace_dir",
                key
            );
        }
        for provider in &ws.auth {
            let normalized = provider.trim().to_ascii_lowercase();
            crate::auth::provider_env_var(&normalized).ok_or_else(|| {
                anyhow::anyhow!(
                    "Enclavefile: workspace.{}.auth contains unsupported provider '{}'",
                    key,
                    provider
                )
            })?;
        }
        for env_token in &ws.env_tokens {
            let normalized = env_token.trim().to_ascii_uppercase();
            crate::auth::provider_for_env_var(&normalized).ok_or_else(|| {
                anyhow::anyhow!(
                    "Enclavefile: workspace.{}.env_tokens contains unsupported token '{}'",
                    key,
                    env_token
                )
            })?;
        }
        let mut parsed_ports = Vec::with_capacity(ws.ports.len());
        for port in &ws.ports {
            parsed_ports.push(
                crate::workspace::PublishedPortSpec::parse(port).map_err(|err| {
                    anyhow::anyhow!(
                        "Enclavefile: workspace.{}.ports contains invalid port '{}': {}",
                        key,
                        port,
                        err
                    )
                })?,
            );
        }
        crate::workspace::validate_published_ports(&parsed_ports).map_err(|err| {
            anyhow::anyhow!("Enclavefile: workspace.{}.ports is invalid: {}", key, err)
        })?;
    }
    Ok(())
}

pub fn scaffold_enclavefile() -> String {
    r#"[sandbox]
name = "devbox"
suite = "bookworm"
# memory_mb = 4096
# cpu_percent = 50
# max_procs = 512

setup = [
  # Commands run inside the sandbox on creation and are re-run by `enclave up`
  # and `enclave restart` so Enclavefile changes can be applied safely.
  # Use idempotent commands (e.g. "apt install -y …") so they are safe to
  # re-run when the Enclavefile changes.
  # Example:
  # "apt install -y nodejs python3",
]

# Define workspaces below. Each [workspace.<id>] block creates a workspace.
#
# [workspace.shell]
# name = "shell"
#
# [workspace.api]
# name = "api"
# run = "node server.js"
# cpu_percent = 25
# memory_mb = 2048
# max_procs = 256
# max_open_files = 65535
# workspace_dir = "./project"
# auth = ["github", "npm"]
# env_tokens = ["ENCLAVE_TOKEN"]
# ports = ["127.0.0.1:3001:3000/tcp"]
"#
    .to_string()
}

pub fn resolve_workspace_host_dir(
    enclavefile_path: &Path,
    raw: &str,
    field_name: &str,
) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("{field_name} must not be empty");
    }
    let base_dir = enclavefile_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("failed to determine Enclavefile parent directory"))?;
    let candidate = if Path::new(trimmed).is_absolute() {
        PathBuf::from(trimmed)
    } else {
        base_dir.join(trimmed)
    };
    let canonical = candidate.canonicalize().with_context(|| {
        format!(
            "failed to resolve {field_name} '{}' (ensure the directory exists and is readable)",
            trimmed
        )
    })?;
    if !canonical.is_dir() {
        bail!(
            "{field_name} '{}' must resolve to an existing directory",
            trimmed
        );
    }
    Ok(canonical.to_string_lossy().to_string())
}
