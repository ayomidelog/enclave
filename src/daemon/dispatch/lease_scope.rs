//! Which lifecycle lease each request needs.
//!
//! Read-only and per-command requests (`status`, `list`, `logs`, `exec`, `cp`,
//! `runtime`, port listing) take no lease, so they keep answering while a
//! lifecycle operation runs. Everything that changes mounts, cgroups, network
//! state, the disk image, or the registry records describing them does.

use std::path::Path;

use anyhow::Result;

use crate::registry::with_registry;
use crate::sandbox::resolve_sandbox_id;
use crate::workspace::workspace_metadata;

use super::Action;
use crate::daemon::leases::LeaseScope;

/// The lease a request needs, or `None` when it needs none.
pub(super) fn scope_for(
    state_dir: &Path,
    action: Action,
    params: &serde_json::Value,
) -> Result<Option<LeaseScope>> {
    let sandbox_scoped = matches!(
        action,
        Action::SandboxCreate
            | Action::SandboxUpdate
            | Action::SandboxStart
            | Action::SandboxStop
            | Action::SandboxPause
            | Action::SandboxResume
            | Action::SandboxDestroy
            | Action::SandboxRemove
            | Action::SandboxExecSetup
            // The bulk start touches every workspace it is given, so it is
            // treated as a sandbox-wide operation rather than one workspace.
            | Action::WorkspaceStartMany
    );
    let workspace_scoped = matches!(
        action,
        Action::WorkspaceCreate
            | Action::WorkspaceStart
            | Action::WorkspaceStop
            | Action::WorkspaceDestroy
            | Action::WorkspaceRemove
            | Action::WorkspaceUpdate
            | Action::WorkspaceResize
            | Action::WorkspaceRestore
            | Action::WorkspaceSnapshot
            | Action::WorkspaceSnapshotGc
            | Action::WorkspaceSnapshotExport
            | Action::WorkspaceSnapshotImport
            | Action::WorkspacePortPublish
            | Action::WorkspacePortUnpublish
    );
    let global_scoped = matches!(
        action,
        Action::SandboxWipe
            | Action::WorkspaceWipe
            | Action::RegistryRepair
            | Action::DaemonDoctorRepair
    );

    if global_scoped {
        return Ok(Some(LeaseScope::Global));
    }
    if !sandbox_scoped && !workspace_scoped {
        return Ok(None);
    }

    let selector = selector_param(params, &["sandbox", "sandbox_id", "name"]);
    let Some(selector) = selector else {
        // The handler will report the missing parameter; without a target there
        // is nothing to serialize against.
        return Ok(None);
    };
    let sandbox = resolve_sandbox_key(state_dir, selector);
    if sandbox_scoped {
        return Ok(Some(LeaseScope::Sandbox(sandbox)));
    }

    let Some(workspace) = selector_param(params, &["workspace", "workspace_id", "name"]) else {
        return Ok(Some(LeaseScope::Sandbox(sandbox)));
    };
    let workspace = resolve_workspace_key(state_dir, &sandbox, workspace);
    Ok(Some(LeaseScope::Workspace { sandbox, workspace }))
}

fn selector_param<'a>(params: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| params.get(*key).and_then(serde_json::Value::as_str))
}

/// The sandbox id a request names, falling back to the raw selector.
///
/// A request can name a sandbox by id or by name, so keying leases by the raw
/// selector would let `sandbox.stop axon-core` and `workspace.stop axon-core ws1`
/// take different leases for the same sandbox. Resolving first keeps them on one
/// key. The fallback covers a sandbox that does not exist yet, which is the
/// normal case for `sandbox.create`.
fn resolve_sandbox_key(state_dir: &Path, selector: &str) -> String {
    with_registry(state_dir, |registry| {
        Ok(resolve_sandbox_id(registry, selector))
    })
    .map_or_else(
        |_| selector.to_string(),
        |resolved| resolved.unwrap_or_else(|_| selector.to_string()),
    )
}

/// The workspace id a request names, falling back to the raw selector.
fn resolve_workspace_key(state_dir: &Path, sandbox: &str, selector: &str) -> String {
    workspace_metadata(state_dir, sandbox, selector)
        .map(|metadata| metadata.id)
        .unwrap_or_else(|_| selector.to_string())
}

#[cfg(test)]
#[path = "../../../tests/src/daemon/lease_scope.rs"]
mod tests;
