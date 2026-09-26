use super::*;

use crate::sandbox::RootfsTier;

pub fn list_sandbox_items(state_dir: &Path) -> Result<Vec<SandboxListItem>> {
    ensure_registry(state_dir)?;
    with_registry(state_dir, |registry| {
        let mut items = Vec::new();
        for entry in registry.sandboxes.values() {
            let mut metadata = entry.metadata.clone();
            normalize_sandbox_metadata(&mut metadata);
            items.push(SandboxListItem {
                id: metadata.id,
                name: metadata.name,
                status: metadata.status,
                workspace_count: entry.workspaces.len(),
            });
        }
        items.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(items)
    })
}

pub fn sandbox_status(state_dir: &Path, selector: &str) -> Result<SandboxStatusReport> {
    with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let entry = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;
        let mut metadata = entry.metadata.clone();
        normalize_sandbox_metadata(&mut metadata);

        let usage = dir_size(Path::new(&metadata.rootfs_path))?;
        Ok(SandboxStatusReport {
            id: metadata.id.clone(),
            name: metadata.name.clone(),
            created_at: metadata.created_at.clone(),
            status: metadata.status.clone(),
            rootfs_path: metadata.rootfs_path.clone(),
            // The tier is the durable choice made when the sandbox was created, and
            // the base path is what the shared tier shares. Both are read from the
            // metadata rather than probed, because the overlay is mounted for the whole
            // life of a running sandbox and absent while it is stopped: a probe would
            // report a different tier for the same sandbox depending on whether it
            // happened to be running.
            rootfs_tier: if metadata.rootfs_lower_path.is_some() {
                RootfsTier::SharedOverlay
            } else {
                RootfsTier::Copied
            },
            rootfs_lower_path: metadata.rootfs_lower_path.clone(),
            rootfs_disk_usage_bytes: usage,
            workspace_count: entry.workspaces.len(),
            limits: metadata.limits.clone(),
        })
    })
}
