//! What a destroy removed, and what it had to leave behind.

use super::super::*;
/// What a workspace destroy removed and what it had to leave behind.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceDestroyReport {
    pub workspace_id: String,
    pub mode: cleanup::CleanupMode,
    /// Resources that are still held, reported in force mode.
    #[serde(default)]
    pub retained: Vec<cleanup::RetainedResource>,
    /// What the host looked like after the destroy finished.
    ///
    /// A removed directory and a removed registry record are not evidence that
    /// the workspace's veth, rules, mounts, or loop device are gone. The
    /// certificate is that evidence, and it is checked against the record as it
    /// was immediately before deletion.
    #[serde(default)]
    pub certificate: crate::workspace::WorkspaceCleanupCertificate,
}

impl WorkspaceDestroyReport {
    /// One line naming everything that was left behind, for command output.
    pub fn retained_summary(&self) -> String {
        self.retained
            .iter()
            .map(|item| format!("{}: {}", item.resource, item.detail))
            .collect::<Vec<_>>()
            .join("; ")
    }
}
