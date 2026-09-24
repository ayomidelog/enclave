use super::*;

pub fn destroy_sandbox(state_dir: &Path, selector: &str) -> Result<String> {
    let (sandbox_id, sandbox) = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;

        Ok((sandbox_id, sandbox.clone()))
    })?;
    let mut journal =
        crate::operation::Journal::begin(state_dir, "sandbox.destroy", sandbox_id.clone())?;
    journal.phase("destroy_workspaces")?;

    if !Path::new(&sandbox.metadata.sandbox_path).exists() {
        let workspace_ids = sandbox.workspaces.keys().cloned().collect::<Vec<_>>();
        let mut errors = Vec::new();
        for workspace_id in workspace_ids {
            if let Err(error) =
                crate::workspace::destroy_workspace(state_dir, &sandbox_id, &workspace_id)
            {
                errors.push(format!("workspace {workspace_id} cleanup: {error:#}"));
            }
        }
        if !errors.is_empty() {
            let error = anyhow!(
                "cannot destroy sandbox '{}' while workspace cleanup is incomplete: {}",
                sandbox_id,
                errors.join("; ")
            );
            let _ = journal.fail(format!("{error:#}"));
            return Err(error);
        }
        if let Err(error) = with_registry_mut(state_dir, |registry| {
            let current = registry
                .sandboxes
                .get(&sandbox_id)
                .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
            if !current.workspaces.is_empty() {
                bail!(
                    "sandbox '{}' still has {} workspace record(s) after cleanup",
                    sandbox_id,
                    current.workspaces.len()
                );
            }
            registry.sandboxes.remove(&sandbox_id);
            Ok(())
        }) {
            let _ = journal.fail(format!("{error:#}"));
            return Err(error);
        }
        journal.succeed()?;
        return Ok(sandbox_id);
    }

    let workspace_ids = sandbox.workspaces.keys().cloned().collect::<Vec<_>>();
    let mut cleanup_errors = Vec::new();
    for workspace_id in workspace_ids {
        if let Err(err) = crate::workspace::destroy_workspace(state_dir, &sandbox_id, &workspace_id)
        {
            cleanup_errors.push(format!("workspace {workspace_id} cleanup: {err:#}"));
        }
    }
    if !cleanup_errors.is_empty() {
        let error = anyhow!(
            "failed to destroy sandbox '{}'; workspace cleanup is incomplete and sandbox resources were retained: {}",
            sandbox_id,
            cleanup_errors.join("; ")
        );
        let _ = journal.fail(format!("{error:#}"));
        return Err(error);
    }

    let sandbox_dir = PathBuf::from(&sandbox.metadata.sandbox_path);
    let mut metadata = sandbox.metadata.clone();
    normalize_sandbox_metadata(&mut metadata);
    let rootfs_unmounted = match mounts::ensure_rootfs_unmounted(&metadata) {
        Ok(()) => true,
        Err(error) => {
            cleanup_errors.push(format!("rootfs cleanup: {error:#}"));
            false
        }
    };
    // The shared-base overlay lives at the rootfs directory itself, so it must
    // be detached before the sandbox directory is removed.
    if let Err(error) = mounts::unmount_rootfs_overlay(&metadata) {
        cleanup_errors.push(format!("rootfs overlay cleanup: {error:#}"));
    }
    let sandbox_cgroup = PathBuf::from("/sys/fs/cgroup")
        .join(crate::sandbox::cgroup::sandbox_cgroup_name(&metadata.id));
    if let Err(err) = crate::sandbox::cgroup::remove_cgroup_path(&sandbox_cgroup) {
        cleanup_errors.push(format!(
            "cgroup {} cleanup: {err:#}",
            sandbox_cgroup.display()
        ));
    }
    if rootfs_unmounted && cleanup_errors.is_empty() {
        let sandboxes_root = sandboxes_dir(state_dir);
        let sandbox_dir =
            crate::fsutil::ensure_path_within(&sandboxes_root, &sandbox_dir, "sandbox directory")?;
        if sandbox_dir.exists() {
            if let Err(error) = fs::remove_dir_all(&sandbox_dir) {
                cleanup_errors.push(format!(
                    "sandbox directory {} cleanup: {error}",
                    sandbox_dir.display()
                ));
            }
        }

        if sandbox_dir.exists() {
            cleanup_errors.push(format!(
                "sandbox directory {} still exists",
                sandbox_dir.display()
            ));
        }
    }

    if !cleanup_errors.is_empty() {
        let error = anyhow!(
            "failed to fully destroy sandbox '{}': {}",
            sandbox_id,
            cleanup_errors.join("; ")
        );
        let _ = journal.fail(format!("{error:#}"));
        return Err(error);
    }

    journal.phase("remove_registry_record")?;
    if let Err(error) = with_registry_mut(state_dir, |registry| {
        let current = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        if !current.workspaces.is_empty() {
            bail!(
                "sandbox '{}' still has {} workspace record(s) after cleanup",
                sandbox_id,
                current.workspaces.len()
            );
        }
        registry.sandboxes.remove(&sandbox_id);
        Ok(())
    }) {
        let _ = journal.fail(format!("{error:#}"));
        return Err(error);
    }
    journal.succeed()?;

    Ok(sandbox_id)
}
