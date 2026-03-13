use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub const LOOPBACK_HOST_IP: &str = "127.0.0.1";
pub const TCP_PROTOCOL: &str = "tcp";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct PublishedPortSpec {
    pub host_ip: String,
    pub host_port: u16,
    pub workspace_port: u16,
    pub protocol: String,
}

impl PublishedPortSpec {
    pub fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            bail!("published port spec must not be empty");
        }

        let (base, protocol_raw) = match trimmed.rsplit_once('/') {
            Some((base, protocol)) => (base, protocol),
            None => (trimmed, TCP_PROTOCOL),
        };
        let protocol = normalize_protocol(protocol_raw, trimmed)?;

        let mut parts = base.split(':');
        let host_ip = parts.next().ok_or_else(|| {
            anyhow::anyhow!(
                "published port spec '{}' must use HOST_IP:HOST_PORT:WORKSPACE_PORT[/tcp]",
                trimmed
            )
        })?;
        let host_port_raw = parts.next().ok_or_else(|| {
            anyhow::anyhow!(
                "published port spec '{}' must use HOST_IP:HOST_PORT:WORKSPACE_PORT[/tcp]",
                trimmed
            )
        })?;
        let workspace_port_raw = parts.next().ok_or_else(|| {
            anyhow::anyhow!(
                "published port spec '{}' must use HOST_IP:HOST_PORT:WORKSPACE_PORT[/tcp]",
                trimmed
            )
        })?;
        if parts.next().is_some() {
            bail!(
                "published port spec '{}' must use HOST_IP:HOST_PORT:WORKSPACE_PORT[/tcp]",
                trimmed
            );
        }

        validate_host_ip(host_ip, trimmed)?;
        let host_port = parse_port(host_port_raw, "host port", trimmed)?;
        let workspace_port = parse_port(workspace_port_raw, "workspace port", trimmed)?;

        Ok(Self {
            host_ip: host_ip.to_string(),
            host_port,
            workspace_port,
            protocol,
        })
    }

    pub fn binding(&self) -> PublishedPortBinding {
        PublishedPortBinding {
            host_ip: self.host_ip.clone(),
            host_port: self.host_port,
            protocol: self.protocol.clone(),
        }
    }
}

impl fmt::Display for PublishedPortSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}/{}",
            self.host_ip, self.host_port, self.workspace_port, self.protocol
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct PublishedPortBinding {
    pub host_ip: String,
    pub host_port: u16,
    pub protocol: String,
}

impl PublishedPortBinding {
    pub fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            bail!("published port binding must not be empty");
        }

        let (base, protocol_raw) = match trimmed.rsplit_once('/') {
            Some((base, protocol)) => (base, protocol),
            None => (trimmed, TCP_PROTOCOL),
        };
        let protocol = normalize_protocol(protocol_raw, trimmed)?;

        let (host_ip, host_port_raw) = base.rsplit_once(':').ok_or_else(|| {
            anyhow::anyhow!(
                "published port binding '{}' must use HOST_IP:HOST_PORT[/tcp]",
                trimmed
            )
        })?;
        validate_host_ip(host_ip, trimmed)?;
        let host_port = parse_port(host_port_raw, "host port", trimmed)?;

        Ok(Self {
            host_ip: host_ip.to_string(),
            host_port,
            protocol,
        })
    }
}

impl fmt::Display for PublishedPortBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}/{}", self.host_ip, self.host_port, self.protocol)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PublishedPortState {
    Configured,
    Active,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishedPortStatus {
    pub host_ip: String,
    pub host_port: u16,
    pub workspace_ip: Option<String>,
    pub workspace_port: u16,
    pub protocol: String,
    pub state: PublishedPortState,
    #[serde(default)]
    pub error: Option<String>,
}

impl PublishedPortStatus {
    pub fn configured(spec: &PublishedPortSpec) -> Self {
        Self {
            host_ip: spec.host_ip.clone(),
            host_port: spec.host_port,
            workspace_ip: None,
            workspace_port: spec.workspace_port,
            protocol: spec.protocol.clone(),
            state: PublishedPortState::Configured,
            error: None,
        }
    }

    pub fn active(spec: &PublishedPortSpec, workspace_ip: &str) -> Self {
        Self {
            host_ip: spec.host_ip.clone(),
            host_port: spec.host_port,
            workspace_ip: Some(workspace_ip.to_string()),
            workspace_port: spec.workspace_port,
            protocol: spec.protocol.clone(),
            state: PublishedPortState::Active,
            error: None,
        }
    }

    pub fn failed(spec: &PublishedPortSpec, error: impl Into<String>) -> Self {
        Self {
            host_ip: spec.host_ip.clone(),
            host_port: spec.host_port,
            workspace_ip: None,
            workspace_port: spec.workspace_port,
            protocol: spec.protocol.clone(),
            state: PublishedPortState::Failed,
            error: Some(error.into()),
        }
    }

    pub fn binding(&self) -> PublishedPortBinding {
        PublishedPortBinding {
            host_ip: self.host_ip.clone(),
            host_port: self.host_port,
            protocol: self.protocol.clone(),
        }
    }
}

pub fn validate_published_ports(specs: &[PublishedPortSpec]) -> Result<()> {
    let mut bindings = BTreeSet::new();
    for spec in specs {
        let binding = spec.binding();
        if !bindings.insert(binding.clone()) {
            bail!(
                "duplicate published host binding '{}:{}' in workspace configuration",
                binding.host_ip,
                binding.host_port
            );
        }
    }
    Ok(())
}

pub fn configured_port_statuses(specs: &[PublishedPortSpec]) -> Vec<PublishedPortStatus> {
    specs.iter().map(PublishedPortStatus::configured).collect()
}

pub fn merge_published_port_statuses(
    specs: &[PublishedPortSpec],
    runtime_statuses: &[PublishedPortStatus],
) -> Vec<PublishedPortStatus> {
    let mut runtime_by_binding = BTreeMap::new();
    for status in runtime_statuses {
        runtime_by_binding.insert(status.binding(), status.clone());
    }

    let mut merged = Vec::with_capacity(specs.len().max(runtime_statuses.len()));
    for spec in specs {
        if let Some(status) = runtime_by_binding.remove(&spec.binding()) {
            merged.push(status);
        } else {
            merged.push(PublishedPortStatus::configured(spec));
        }
    }

    merged.extend(runtime_by_binding.into_values());
    merged
}

fn normalize_protocol(raw: &str, context: &str) -> Result<String> {
    let protocol = raw.trim().to_ascii_lowercase();
    if protocol != TCP_PROTOCOL {
        bail!(
            "published port spec '{}' uses unsupported protocol '{}'; only tcp is supported",
            context,
            raw
        );
    }
    Ok(protocol)
}

fn validate_host_ip(host_ip: &str, context: &str) -> Result<()> {
    if host_ip != LOOPBACK_HOST_IP {
        bail!(
            "published port spec '{}' uses unsupported host IP '{}'; only {} is allowed",
            context,
            host_ip,
            LOOPBACK_HOST_IP
        );
    }
    Ok(())
}

fn parse_port(raw: &str, label: &str, context: &str) -> Result<u16> {
    let port = raw.parse::<u16>().map_err(|_| {
        anyhow::anyhow!(
            "published port spec '{}' has invalid {} '{}'",
            context,
            label,
            raw
        )
    })?;
    if port == 0 {
        bail!(
            "published port spec '{}' has invalid {} '{}'",
            context,
            label,
            raw
        );
    }
    Ok(port)
}
