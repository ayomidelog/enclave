use super::*;

pub fn start_sandbox(state_dir: &Path, selector: &str) -> Result<SandboxMetadata> {
    let mut journal =
        crate::operation::Journal::begin(state_dir, "sandbox.start", selector.to_string())?;
    journal.phase("mount_rootfs")?;
    let metadata = match with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let entry = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;
        normalize_sandbox_metadata(&mut entry.metadata);
        ensure_sandbox_layout(&entry.metadata)?;
        mounts::ensure_rootfs_mounted(&entry.metadata)?;
        entry.metadata.status = SandboxStatus::Running;
        persist_sandbox_metadata(&entry.metadata)?;
        Ok(entry.metadata.clone())
    }) {
        Ok(metadata) => metadata,
        Err(error) => {
            let _ = journal.fail(format!("{error:#}"));
            return Err(error);
        }
    };
    journal.phase("sync_runtime_limits")?;
    if let Err(error) = crate::workspace::sync_sandbox_runtime_limits(state_dir, &metadata.id) {
        let _ = journal.fail(format!("{error:#}"));
        return Err(error);
    }
    journal.succeed()?;
    Ok(metadata)
}
