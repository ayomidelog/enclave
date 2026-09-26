use super::*;

pub fn stop_sandbox(state_dir: &Path, selector: &str) -> Result<SandboxMetadata> {
    let mut journal =
        crate::operation::Journal::begin(state_dir, "sandbox.stop", selector.to_string())?;
    journal.phase("mark_stopping")?;
    // Record the in-flight transition durably so a crash before the rootfs is
    // unmounted is visible to the next daemon start, which completes the stop.
    let sandbox_id = with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let entry = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;
        entry.metadata.status = SandboxStatus::Stopping;
        persist_sandbox_metadata(&entry.metadata)?;
        Ok(sandbox_id)
    })?;
    journal.phase("unmount_rootfs")?;
    // Unmounting is a slow host operation, so it runs outside the registry lock:
    // holding the lock across it would stall every other sandbox's requests for
    // the duration. The snapshot is read under the lock and the result committed
    // under it again.
    let snapshot = with_registry(state_dir, |registry| {
        let entry = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;
        let mut metadata = entry.metadata.clone();
        normalize_sandbox_metadata(&mut metadata);
        Ok(metadata)
    })?;
    if let Err(error) = mounts::ensure_rootfs_unmounted(&snapshot) {
        let _ = journal.fail(format!("{error:#}"));
        return Err(error);
    }
    let metadata = match with_registry_mut(state_dir, |registry| {
        let entry = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;
        // Normalize here too, so the persisted metadata carries the resolved
        // paths exactly as it did when the unmount ran under the lock.
        normalize_sandbox_metadata(&mut entry.metadata);
        entry.metadata.status = SandboxStatus::Stopped;
        persist_sandbox_metadata(&entry.metadata)?;
        Ok(entry.metadata.clone())
    }) {
        Ok(metadata) => metadata,
        Err(error) => {
            let _ = journal.fail(format!("{error:#}"));
            return Err(error);
        }
    };
    journal.phase("remove_sandbox_cgroup")?;
    let sandbox_cgroup = PathBuf::from("/sys/fs/cgroup")
        .join(crate::sandbox::cgroup::sandbox_cgroup_name(&metadata.id));
    if let Err(err) = crate::sandbox::cgroup::remove_cgroup_path(&sandbox_cgroup) {
        let _ = journal.fail(format!("sandbox cgroup cleanup failed: {err:#}"));
        return Err(err).context("failed to remove sandbox cgroup");
    }
    journal.succeed()?;
    Ok(metadata)
}

pub fn pause_sandbox(state_dir: &Path, selector: &str) -> Result<SandboxMetadata> {
    let mut journal =
        crate::operation::Journal::begin(state_dir, "sandbox.pause", selector.to_string())?;
    let metadata = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let entry = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow::anyhow!("sandbox '{}' not found", selector))?;
        if entry.metadata.status != SandboxStatus::Running {
            // The state is named, not just the requirement: "not running" reads the
            // same for a sandbox that is stopped and one that is already paused, and
            // the two call for different commands.
            bail!(
                "sandbox '{}' is {}; pause needs it running (start it with `enclave start {}`)",
                selector,
                entry.metadata.status.as_str(),
                selector
            );
        }
        Ok(entry.metadata.clone())
    })?;

    journal.phase("freeze_workspaces")?;
    if let Err(error) =
        crate::workspace::freeze_workspaces_in_sandbox(state_dir, &metadata.id, true)
    {
        let _ = journal.fail(format!("{error:#}"));
        return Err(error);
    }
    let commit = with_registry_mut(state_dir, |registry| {
        let entry = registry
            .sandboxes
            .get_mut(&metadata.id)
            .ok_or_else(|| anyhow::anyhow!("sandbox '{}' not found", metadata.id))?;
        entry.metadata.status = SandboxStatus::Paused;
        persist_sandbox_metadata(&entry.metadata)?;
        Ok(entry.metadata.clone())
    });
    if let Err(error) = commit {
        if let Err(rollback_error) =
            crate::workspace::freeze_workspaces_in_sandbox(state_dir, &metadata.id, false)
        {
            tracing::error!(
                "failed to roll back sandbox pause after registry error: {rollback_error:#}"
            );
        }
        let _ = journal.fail(format!("{error:#}"));
        return Err(error);
    }
    let result = commit?;
    journal.succeed()?;
    Ok(result)
}

pub fn resume_sandbox(state_dir: &Path, selector: &str) -> Result<SandboxMetadata> {
    let mut journal =
        crate::operation::Journal::begin(state_dir, "sandbox.resume", selector.to_string())?;
    let metadata = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let entry = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow::anyhow!("sandbox '{}' not found", selector))?;
        if entry.metadata.status != SandboxStatus::Paused {
            bail!(
                "sandbox '{}' is {}; resume needs it paused (pause it with `enclave pause {}`)",
                selector,
                entry.metadata.status.as_str(),
                selector
            );
        }
        Ok(entry.metadata.clone())
    })?;

    journal.phase("thaw_workspaces")?;
    if let Err(error) =
        crate::workspace::freeze_workspaces_in_sandbox(state_dir, &metadata.id, false)
    {
        let _ = journal.fail(format!("{error:#}"));
        return Err(error);
    }
    let commit = with_registry_mut(state_dir, |registry| {
        let entry = registry
            .sandboxes
            .get_mut(&metadata.id)
            .ok_or_else(|| anyhow::anyhow!("sandbox '{}' not found", metadata.id))?;
        entry.metadata.status = SandboxStatus::Running;
        persist_sandbox_metadata(&entry.metadata)?;
        Ok(entry.metadata.clone())
    });
    if let Err(error) = commit {
        if let Err(rollback_error) =
            crate::workspace::freeze_workspaces_in_sandbox(state_dir, &metadata.id, true)
        {
            tracing::error!(
                "failed to roll back sandbox resume after registry error: {rollback_error:#}"
            );
        }
        let _ = journal.fail(format!("{error:#}"));
        return Err(error);
    }
    let result = commit?;
    journal.succeed()?;
    Ok(result)
}
