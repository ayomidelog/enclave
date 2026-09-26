//! Turning a workspace selector into a workspace id.

use anyhow::Result;

use crate::registry::RegistrySandbox;

pub(crate) fn resolve_workspace_id(sandbox: &RegistrySandbox, selector: &str) -> Result<String> {
    if sandbox.workspaces.contains_key(selector) {
        return Ok(selector.to_string());
    }

    let mut matches = Vec::new();
    for (id, workspace) in &sandbox.workspaces {
        if workspace.name == selector {
            matches.push(id.clone());
        }
    }

    match matches.len() {
        0 => Err(crate::error::coded(
            crate::error::ErrorCode::NotFound,
            format!(
                "workspace '{}' not found in sandbox '{}'",
                selector, sandbox.metadata.id
            ),
        )),
        1 => Ok(matches.remove(0)),
        _ => Err(crate::error::coded(
            crate::error::ErrorCode::Conflict,
            format!(
                "workspace name '{}' is ambiguous in sandbox '{}'; use id instead (matches: {})",
                selector,
                sandbox.metadata.id,
                matches.join(", ")
            ),
        )),
    }
}
