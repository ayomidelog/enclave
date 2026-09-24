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
