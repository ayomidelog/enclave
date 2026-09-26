//! Freezing and thawing every workspace in a sandbox.
//!
//! A sandbox pause is one lifecycle boundary: the whole sandbox cgroup is frozen
//! together so no workspace can make progress while another is stopped, and the
//! published ports are withdrawn and restored around it.

use super::*;

pub(crate) fn freeze_workspaces_in_sandbox(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    frozen: bool,
) -> Result<()> {
    let (sandbox_id, has_active_workspace) = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_selector))?;
        Ok((
            sandbox_id,
            sandbox
                .workspaces
                .values()
                .filter(|workspace| workspace.status.may_have_runtime())
                .count()
                > 0,
        ))
    })?;

    if !has_active_workspace {
        return Ok(());
    }
    let cgroup_path = std::path::PathBuf::from("/sys/fs/cgroup")
        .join(crate::sandbox::cgroup::sandbox_cgroup_name(&sandbox_id));
    if !crate::sandbox::cgroup::set_cgroup_frozen(&cgroup_path, frozen)? {
        bail!(
            "sandbox cgroup {} does not support freezing",
            cgroup_path.display()
        );
    }
    Ok(())
}
