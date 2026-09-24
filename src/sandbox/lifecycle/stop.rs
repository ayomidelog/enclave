use super::*;

pub fn stop_sandbox(state_dir: &Path, selector: &str) -> Result<SandboxMetadata> {
    let mut journal =
        crate::operation::Journal::begin(state_dir, "sandbox.stop", selector.to_string())?;
    journal.phase("unmount_rootfs")?;
    let metadata = match with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let entry = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;
        normalize_sandbox_metadata(&mut entry.metadata);
        mounts::ensure_rootfs_unmounted(&entry.metadata)?;
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
            bail!("sandbox '{}' is not running", selector);
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
            bail!("sandbox '{}' is not paused", selector);
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
