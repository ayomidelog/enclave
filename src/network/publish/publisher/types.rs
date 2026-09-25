//! The publisher, the state it holds, and the error a failed bind reports.

use super::*;

/// One workspace the publisher is serving ports for, and the ports it holds.
#[derive(Debug, Clone)]
pub struct PublishedPortOwner {
    pub sandbox_id: String,
    pub workspace_id: String,
    pub ports: Vec<PublishedPortStatus>,
}

pub struct PortPublisher {
    pub(in crate::network::publish) inner: Mutex<PublisherState>,
    pub(super) connections: Arc<ConnectionLimiter>,
}
impl Default for PortPublisher {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
pub(in crate::network::publish) struct PublisherState {
    pub(super) active: BTreeMap<WorkspacePublishKey, Vec<ActivePublication>>,
    pub(super) failed: BTreeMap<WorkspacePublishKey, Vec<PublishedPortStatus>>,
}

impl std::fmt::Debug for PortPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state();
        f.debug_struct("PortPublisher")
            .field("active_workspaces", &state.active.len())
            .field("failed_workspaces", &state.failed.len())
            .finish()
    }
}

pub(in crate::network::publish) fn publish_bind_error(
    spec: &PublishedPortSpec,
    err: io::Error,
) -> anyhow::Error {
    if err.kind() == io::ErrorKind::AddrInUse {
        return anyhow::anyhow!(
            "failed to publish {}:{} -> workspace port {}: host port already in use",
            spec.host_ip,
            spec.host_port,
            spec.workspace_port
        );
    }

    anyhow::anyhow!(
        "failed to publish {}:{} -> workspace port {}: {}",
        spec.host_ip,
        spec.host_port,
        spec.workspace_port,
        err
    )
}
