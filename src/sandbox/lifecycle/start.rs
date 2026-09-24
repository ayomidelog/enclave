use super::*;

pub fn start_sandbox(state_dir: &Path, selector: &str) -> Result<SandboxMetadata> {
    let mut journal =
        crate::operation::Journal::begin(state_dir, "sandbox.start", selector.to_string())?;
    journal.phase("mark_starting")?;
    // Record the in-flight transition durably: a crash between mounting the
    // rootfs and committing `running` then leaves evidence that the next
    // daemon start can roll back, instead of a stopped-looking sandbox with a
    // mounted rootfs.
    let sandbox_id = with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let entry = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;
        if entry.metadata.status.is_transitional() {
            bail!(
                "sandbox '{}' is {:?}; wait for the current operation to finish",
                entry.metadata.id,
                entry.metadata.status
            );
        }
        entry.metadata.status = SandboxStatus::Starting;
        persist_sandbox_metadata(&entry.metadata)?;
        Ok(sandbox_id)
    })?;
    journal.phase("mount_rootfs")?;
    let metadata = match with_registry_mut(state_dir, |registry| {
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
            rollback_failed_start(state_dir, &sandbox_id);
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

/// Undo a sandbox start that did not reach `running`.
///
/// The rootfs bind mount may already exist, so it is unmounted before the
/// status is rolled back to `stopped`. Failures are logged rather than
/// propagated: the caller is already returning the original start error, and
/// the next daemon start reconciles any state left behind.
fn rollback_failed_start(state_dir: &Path, sandbox_id: &str) {
    let result = with_registry_mut(state_dir, |registry| {
        let entry = registry
            .sandboxes
            .get_mut(sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        if let Err(error) = mounts::ensure_rootfs_unmounted(&entry.metadata) {
            tracing::warn!(
                "sandbox '{}': failed to unmount rootfs after failed start: {error:#}",
                sandbox_id
            );
        }
        entry.metadata.status = SandboxStatus::Stopped;
        persist_sandbox_metadata(&entry.metadata)
    });
    if let Err(error) = result {
        tracing::warn!(
            "sandbox '{}': failed to roll back status after failed start: {error:#}",
            sandbox_id
        );
    }
}
