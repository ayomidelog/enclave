//! What the publisher is serving right now.
//!
//! The listeners are owned by threads rather than by any file on disk, so this is the
//! only authority on which host ports Enclave holds. A stop uses it to prove the
//! listeners were released, and doctor uses it to name one whose workspace is gone.

use super::*;

impl PortPublisher {
    /// Whether any published port for a workspace is still serving.
    ///
    /// `clear_workspace_ports` returns nothing, so a caller that wants to prove
    /// the listeners were released has to look at the publisher afterwards. This
    /// is what lets a stop certificate cover ports rather than assume them.
    pub fn has_active_workspace_ports(&self, sandbox_id: &str, workspace_id: &str) -> bool {
        let key = WorkspacePublishKey::new(sandbox_id, workspace_id);
        let state = self.state();
        state
            .active
            .get(&key)
            .is_some_and(|publications| !publications.is_empty())
    }

    /// Every workspace the publisher is currently serving ports for.
    ///
    /// The publisher is the only authority on which host ports Enclave holds:
    /// the listeners are owned by threads rather than by any file on disk, so a
    /// caller that wants to prove nothing was left behind has to ask here. Doctor
    /// uses this to name a listener whose workspace is no longer running, which
    /// is otherwise invisible until a later start fails with the port in use.
    pub fn active_workspaces(&self) -> Vec<PublishedPortOwner> {
        let state = self.state();
        state
            .active
            .iter()
            .map(|(key, publications)| PublishedPortOwner {
                sandbox_id: key.sandbox_id.clone(),
                workspace_id: key.workspace_id.clone(),
                ports: publications.iter().map(ActivePublication::status).collect(),
            })
            .collect()
    }

    pub fn workspace_statuses(
        &self,
        sandbox_id: &str,
        workspace_id: &str,
    ) -> Vec<PublishedPortStatus> {
        let key = WorkspacePublishKey::new(sandbox_id, workspace_id);
        let state = self.state();

        let mut statuses = Vec::new();
        if let Some(active) = state.active.get(&key) {
            statuses.extend(active.iter().map(ActivePublication::status));
        }
        if let Some(failed) = state.failed.get(&key) {
            statuses.extend(failed.iter().cloned());
        }
        statuses.sort_by_key(|status| {
            (
                status.host_ip.clone(),
                status.host_port,
                status.workspace_port,
                status.protocol.clone(),
            )
        });
        statuses
    }
}
