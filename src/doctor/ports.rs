//! Reporting published host ports that no running workspace is using.
//!
//! A published port is a listener on the host, held by a thread in the daemon
//! rather than by anything on disk. That makes it the one resource Enclave owns
//! that no filesystem scan can find, and the one a stale daemon leaves behind
//! invisibly: the host port stays bound, and the next start of that workspace
//! fails with the port already in use, naming a process the operator cannot see.
//!
//! The publisher is asked what it is serving, and each entry is compared with
//! the registry. Three mismatches are reported, because they have different
//! causes and different fixes:
//!
//! - a listener whose workspace is no longer running, which a stop should have
//!   released;
//! - a listener whose workspace no longer declares those ports, which an update
//!   should have released;
//! - a listener whose workspace is not registered at all, which a destroy should
//!   have released.
//!
//! None of them is removed here. Releasing a listener means shutting down a
//! thread and closing the connections it is proxying, which is the lifecycle's
//! job; doctor reports it so the operator can stop the workspace or restart the
//! daemon.

use std::collections::BTreeMap;
use std::path::Path;

use crate::network::publish::{PortPublisher, PublishedPortOwner};

use super::DoctorCheck;

/// What the registry says about one workspace's published ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExpectedPorts {
    /// The workspace is running and declares the ports it is serving.
    Running,
    /// The workspace exists but is not running, so nothing should be listening.
    Stopped,
    /// The workspace is running but no longer declares a published port.
    Undeclared,
}

/// Compare what the publisher is serving with what the registry expects.
pub(crate) fn check_published_ports(state_dir: &Path, publisher: &PortPublisher) -> DoctorCheck {
    const NAME: &str = "published_ports";

    let expected = match expected_publishers(state_dir) {
        Ok(expected) => expected,
        Err(err) => {
            return DoctorCheck::warn(
                NAME,
                &format!("failed to read workspace port ownership: {err:#}"),
            )
        }
    };

    let serving = publisher.active_workspaces();
    let details = port_inventory_details(&expected, &serving);
    if details.is_empty() {
        let held = serving.iter().map(|owner| owner.ports.len()).sum::<usize>();
        return DoctorCheck::ok(
            NAME,
            &format!(
                "{held} published port(s) across {} workspace(s), all expected",
                serving.len()
            ),
        );
    }
    DoctorCheck::warn(NAME, &details.join("; "))
}

/// Name every listener the registry does not account for.
///
/// Split from the registry read so the classification can be exercised on its
/// own: it is the part with the rules, and it needs neither a state directory
/// nor a bound port.
pub(super) fn port_inventory_details(
    expected: &BTreeMap<(String, String), ExpectedPorts>,
    serving: &[PublishedPortOwner],
) -> Vec<String> {
    let mut details = Vec::new();
    for owner in serving {
        let key = (owner.sandbox_id.clone(), owner.workspace_id.clone());
        let count = owner.ports.len();
        let detail = match expected.get(&key) {
            Some(ExpectedPorts::Running) => continue,
            Some(ExpectedPorts::Stopped) => format!(
                "{count} port(s) for workspace {} in sandbox {} are still listening while the workspace is not running",
                owner.workspace_id, owner.sandbox_id
            ),
            Some(ExpectedPorts::Undeclared) => format!(
                "{count} port(s) for workspace {} in sandbox {} are listening but the workspace no longer declares them",
                owner.workspace_id, owner.sandbox_id
            ),
            None => format!(
                "{count} port(s) for workspace {} in sandbox {} are listening but the workspace is not registered",
                owner.workspace_id, owner.sandbox_id
            ),
        };
        details.push(detail);
    }
    details
}

fn expected_publishers(
    state_dir: &Path,
) -> anyhow::Result<BTreeMap<(String, String), ExpectedPorts>> {
    crate::registry::with_registry(state_dir, |registry| {
        let mut expected = BTreeMap::new();
        for sandbox in registry.sandboxes.values() {
            for workspace in sandbox.workspaces.values() {
                let active = crate::workspace::workspace_runtime_is_active(workspace);
                let declared = !workspace.published_ports.is_empty();
                let state = match (active, declared) {
                    (true, true) => ExpectedPorts::Running,
                    (true, false) => ExpectedPorts::Undeclared,
                    (false, _) => ExpectedPorts::Stopped,
                };
                expected.insert((sandbox.metadata.id.clone(), workspace.id.clone()), state);
            }
        }
        Ok(expected)
    })
}

#[cfg(test)]
#[path = "../../tests/src/doctor/ports.rs"]
mod tests;
