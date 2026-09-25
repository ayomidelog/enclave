//! The sandbox cgroup, which is created for the workspaces it holds and removed
//! once none of them can own a runtime.

use crate::registry::RegistrySandbox;
use crate::sandbox::SandboxMetadata;

pub(crate) fn remove_sandbox_cgroup_if_idle(sandbox: &RegistrySandbox) {
    if sandbox
        .workspaces
        .values()
        .all(|item| !item.status.may_have_runtime())
        && !sandbox.metadata.limits.has_limits()
    {
        remove_sandbox_cgroup(&sandbox.metadata);
    }
}

pub(crate) fn remove_sandbox_cgroup(sandbox: &SandboxMetadata) {
    let sandbox_path = std::path::PathBuf::from("/sys/fs/cgroup")
        .join(crate::sandbox::cgroup::sandbox_cgroup_name(&sandbox.id));
    if let Err(err) = crate::sandbox::cgroup::remove_cgroup_path(&sandbox_path) {
        tracing::debug!(
            "sandbox cgroup cleanup skipped for '{}': {err:#}",
            sandbox.id
        );
    }
}
