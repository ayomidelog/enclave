use anyhow::{anyhow, bail, Result};

use crate::workspace::{CleanupMode, RetainedResource};

use super::*;

pub fn destroy_sandbox(state_dir: &Path, selector: &str) -> Result<String> {
    destroy_sandbox_with_mode(state_dir, selector, CleanupMode::Normal)
        .map(|report| report.sandbox_id)
}

/// What a sandbox destroy removed and what it had to leave behind.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SandboxDestroyReport {
    pub sandbox_id: String,
    pub mode: CleanupMode,
    /// Resources that are still held, reported in force mode.
    #[serde(default)]
    pub retained: Vec<RetainedResource>,
}

impl SandboxDestroyReport {
    /// One line naming everything that was left behind, for command output.
    pub fn retained_summary(&self) -> String {
        self.retained
            .iter()
            .map(|item| format!("{}: {}", item.resource, item.detail))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Destroy a sandbox and report what was released and what was retained.
///
/// Normal mode fails while any workspace or sandbox resource is still held, so
/// the registry record survives as recovery evidence. Force mode removes the
/// record either way and reports what it could not release, leaving the host
/// state to `enclave doctor --repair`.
///
/// The sandbox directory is only removed when nothing is still held. A live
/// mount, runtime, or cgroup keeps its files in place because the directory is
/// the last description of what is running.
pub fn destroy_sandbox_with_mode(
    state_dir: &Path,
    selector: &str,
    mode: CleanupMode,
) -> Result<SandboxDestroyReport> {
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

    let mut retained: Vec<RetainedResource> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for workspace_id in sandbox.workspaces.keys() {
        match crate::workspace::destroy_workspace_with_mode(
            state_dir,
            &sandbox_id,
            workspace_id,
            mode,
        ) {
            Ok(report) => {
                retained.extend(
                    report
                        .retained
                        .into_iter()
                        .map(|resource| RetainedResource {
                            resource: format!("workspace {workspace_id} {}", resource.resource),
                            detail: resource.detail,
                        }),
                )
            }
            Err(error) => errors.push(format!("workspace {workspace_id} cleanup: {error:#}")),
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

    if Path::new(&sandbox.metadata.sandbox_path).exists() {
        journal.phase("release_sandbox_resources")?;
        let sandbox_dir = PathBuf::from(&sandbox.metadata.sandbox_path);
        let mut metadata = sandbox.metadata.clone();
        normalize_sandbox_metadata(&mut metadata);
        if let Err(error) = mounts::ensure_rootfs_unmounted(&metadata) {
            retained.push(RetainedResource {
                resource: "rootfs".to_string(),
                detail: format!("{error:#}"),
            });
        }
        // The shared-base overlay lives at the rootfs directory itself, so it
        // must be detached before the sandbox directory is removed.
        if let Err(error) = mounts::unmount_rootfs_overlay(&metadata) {
            retained.push(RetainedResource {
                resource: "rootfs overlay".to_string(),
                detail: format!("{error:#}"),
            });
        }
        // A mount under the sandbox that Enclave did not create, for example a
        // tmpfs an operator placed there, would be walked into by the directory
        // removal below. Refuse instead: only the operator can decide when that
        // mount goes.
        if let Ok(snapshot) = crate::fsutil::MountInfoSnapshot::load() {
            let foreign = snapshot.foreign_at_or_below(&sandbox_dir);
            if !foreign.is_empty() {
                retained.push(RetainedResource {
                    resource: "foreign_mounts".to_string(),
                    detail: format!(
                        "{} mount(s) below the sandbox were not created by Enclave and were left in place: {}",
                        foreign.len(),
                        foreign.join("; ")
                    ),
                });
            }
        }
        let sandbox_cgroup = PathBuf::from("/sys/fs/cgroup")
            .join(crate::sandbox::cgroup::sandbox_cgroup_name(&metadata.id));
        if let Err(error) = crate::sandbox::cgroup::remove_cgroup_path(&sandbox_cgroup) {
            retained.push(RetainedResource {
                resource: format!("cgroup {}", sandbox_cgroup.display()),
                detail: format!("{error:#}"),
            });
        }

        if retained.is_empty() {
            journal.phase("remove_sandbox_directory")?;
            let sandboxes_root = sandboxes_dir(state_dir);
            let sandbox_dir = crate::fsutil::ensure_path_within(
                &sandboxes_root,
                &sandbox_dir,
                "sandbox directory",
            )?;
            if sandbox_dir.exists() {
                if let Err(error) = fs::remove_dir_all(&sandbox_dir) {
                    errors.push(format!(
                        "sandbox directory {} cleanup: {error}",
                        sandbox_dir.display()
                    ));
                }
            }
            if sandbox_dir.exists() {
                errors.push(format!(
                    "sandbox directory {} still exists",
                    sandbox_dir.display()
                ));
            }
        }
    }

    if !errors.is_empty() {
        let error = anyhow!(
            "failed to fully destroy sandbox '{}': {}",
            sandbox_id,
            errors.join("; ")
        );
        let _ = journal.fail(format!("{error:#}"));
        return Err(error);
    }

    if !retained.is_empty() && !mode.is_force() {
        let error = anyhow!(
            "failed to fully destroy sandbox '{}': sandbox resources were retained: {}",
            sandbox_id,
            SandboxDestroyReport {
                sandbox_id: sandbox_id.clone(),
                mode,
                retained: retained.clone(),
            }
            .retained_summary()
        );
        let _ = journal.fail(format!("{error:#}"));
        return Err(error);
    }

    if !retained.is_empty() {
        // Force mode drops the record and leaves the host state to
        // `doctor --repair`, so the journal has to name what was left behind.
        journal.phase("retain_unreleased_resources")?;
        tracing::warn!(
            "sandbox '{}' destroy left resources behind: {}",
            sandbox_id,
            SandboxDestroyReport {
                sandbox_id: sandbox_id.clone(),
                mode,
                retained: retained.clone(),
            }
            .retained_summary()
        );
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

    Ok(SandboxDestroyReport {
        sandbox_id,
        mode,
        retained,
    })
}
