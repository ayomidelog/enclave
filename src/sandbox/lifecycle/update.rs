use super::*;

pub fn update_sandbox_limits(
    state_dir: &Path,
    selector: &str,
    limits: &SandboxLimitsUpdate,
) -> Result<SandboxMetadata> {
    with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let entry = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;
        // A budget below what the sandbox already allocates is refused rather than
        // stored: the workspaces holding that space already exist, so the sandbox
        // would be over its own limit the moment it was set and nothing would bring it
        // back inside until one of them was shrunk or destroyed.
        if let Some(budget) = limits.disk_bytes {
            let mut prospective = entry.metadata.limits.clone();
            prospective.disk_bytes = budget;
            prospective
                .check_disk_budget_covers(crate::workspace::sandbox_workspace_disk_bytes(entry))?;
        }
        if entry.metadata.limits.apply_update(limits)? {
            persist_sandbox_metadata(&entry.metadata)?;
        }
        Ok(entry.metadata.clone())
    })
}

pub(crate) fn persist_sandbox_metadata(metadata: &SandboxMetadata) -> Result<()> {
    let metadata_path = PathBuf::from(&metadata.sandbox_path).join("sandbox.json");
    let metadata_json = serde_json::to_string_pretty(metadata)?;
    crate::fsutil::write_file_atomic(&metadata_path, metadata_json.as_bytes(), 0o600).with_context(
        || {
            format!(
                "failed to write sandbox metadata {}",
                metadata_path.display()
            )
        },
    )
}
