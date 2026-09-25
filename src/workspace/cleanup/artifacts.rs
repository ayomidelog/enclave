use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::network;
use crate::sandbox::SandboxMetadata;

use super::super::session;
use super::super::types::WorkspaceMetadata;
use super::{remove_workspace_cgroups, CleanupMode, CleanupOutcome, RetainedResource};

/// Release every host resource a workspace owns, then remove its files.
///
/// In normal mode any resource that cannot be released is an error and nothing
/// is deleted, so the caller keeps its recovery evidence. In force mode the
/// resources that could not be released are reported instead, and the files are
/// kept whenever anything is still held: removing a directory out from under a
/// live mount or a live runtime would either fail or lose the only remaining
/// description of what is running.
pub(crate) fn cleanup_workspace_artifacts(
    sandbox: &SandboxMetadata,
    workspace: &WorkspaceMetadata,
    mode: CleanupMode,
) -> Result<CleanupOutcome> {
    let workspace_path = workspace_dir_for_cleanup(sandbox, workspace)?;
    let mut retained: Vec<RetainedResource> = Vec::new();

    if let Some(pid) = workspace.runtime_pid {
        match session::stop_session(pid, workspace.runtime_starttime_ticks) {
            Ok(()) => {}
            Err(error) => retained.push(RetainedResource {
                resource: "runtime".to_string(),
                detail: format!("pid {pid} did not stop: {error:#}"),
            }),
        }
        if session::process_matches(pid, workspace.runtime_starttime_ticks) {
            retained.push(RetainedResource {
                resource: "runtime".to_string(),
                detail: format!("pid {pid} is still alive after stop"),
            });
        }
        if let Err(error) = remove_workspace_cgroups(sandbox, &workspace.id, Some(pid)) {
            retained.push(RetainedResource {
                resource: "cgroups".to_string(),
                detail: format!("{error:#}"),
            });
        }
    }
    if let Some(ip) = workspace.assigned_ip.as_deref() {
        let report = network::teardown_workspace_network(ip, &workspace.id);
        if !report.is_complete() {
            retained.push(RetainedResource {
                resource: "network".to_string(),
                detail: report
                    .failures
                    .iter()
                    .map(|failure| format!("{}: {}", failure.resource, failure.message))
                    .collect::<Vec<_>>()
                    .join("; "),
            });
        }
    }
    if let Err(error) = crate::workspace::ensure_workspace_storage_unmounted(workspace) {
        retained.push(RetainedResource {
            resource: "mounts".to_string(),
            detail: format!("{error:#}"),
        });
    }
    // A mount Enclave did not create, sitting below the workspace, is not a
    // resource Enclave may release, but deleting the workspace directory would
    // recurse into it and remove the files behind it. Refuse instead: the
    // operator placed that mount, so the operator decides when it goes.
    if let Ok(snapshot) = crate::fsutil::MountInfoSnapshot::load() {
        let foreign = snapshot.foreign_at_or_below(Path::new(&workspace.workspace_path));
        if !foreign.is_empty() {
            retained.push(RetainedResource {
                resource: "foreign_mounts".to_string(),
                detail: format!(
                    "{} mount(s) below the workspace were not created by Enclave and were left in place: {}",
                    foreign.len(),
                    foreign.join("; ")
                ),
            });
        }
    }

    if !retained.is_empty() && !mode.is_force() {
        return Err(crate::error::coded(
            crate::error::ErrorCode::CleanupIncomplete,
            format!(
                "workspace '{}' cleanup incomplete, retaining workspace files and registry record: {}",
                workspace.id,
                CleanupOutcome {
                    files_removed: false,
                    retained: retained.clone(),
                }
                .retained_summary()
            ),
        ));
    }

    if !retained.is_empty() {
        return Ok(CleanupOutcome {
            files_removed: false,
            retained,
        });
    }

    if let Some(workspace_path) = workspace_path {
        for artifact in [
            workspace_path.join("workspace.json"),
            workspace_path.join("fs.img"),
            workspace_path.join("ns"),
            workspace_path.join("home-upper"),
            workspace_path.join("home-work"),
            workspace_path.join("home-merged"),
            workspace_path.join("runtime"),
            workspace_path.join("fs"),
        ] {
            remove_path_if_present(&artifact)?;
        }
        remove_path_if_present(&workspace_path)?;
        if workspace_path.exists() {
            bail!(
                "workspace directory {} still exists",
                workspace_path.display()
            );
        }
    }

    Ok(CleanupOutcome {
        files_removed: true,
        retained: Vec::new(),
    })
}

/// Resolve the workspace directory, refusing anything that is not exactly where
/// the sandbox layout says it should be.
///
/// The checks run before anything is deleted so a symlinked or relocated path
/// cannot be used to remove unrelated data.
fn workspace_dir_for_cleanup(
    sandbox: &SandboxMetadata,
    workspace: &WorkspaceMetadata,
) -> Result<Option<PathBuf>> {
    if workspace.id.is_empty()
        || !workspace
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        bail!("workspace id is unsafe for cleanup: {}", workspace.id);
    }
    let sandbox_path = PathBuf::from(&sandbox.sandbox_path);
    let workspaces_root = PathBuf::from(&sandbox.workspaces_path);
    let workspace_path = PathBuf::from(&workspace.workspace_path);
    if workspaces_root != sandbox_path.join("workspaces")
        || workspace_path != workspaces_root.join(&workspace.id)
    {
        bail!(
            "workspace '{}' paths do not match their sandbox layout",
            workspace.id
        );
    }
    let sandbox_metadata = match fs::symlink_metadata(&sandbox_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to inspect sandbox path {}", sandbox_path.display())
            })
        }
    };
    if sandbox_metadata.file_type().is_symlink() {
        bail!("sandbox path {} is a symlink", sandbox_path.display());
    }
    let sandbox_root = fs::canonicalize(&sandbox.sandbox_path)
        .with_context(|| format!("failed to resolve sandbox path {}", sandbox.sandbox_path))?;
    let expected_root = sandbox_root.join("workspaces");
    if workspaces_root != expected_root {
        bail!(
            "sandbox '{}' workspace root {} does not match expected path {}",
            sandbox.id,
            workspaces_root.display(),
            expected_root.display()
        );
    }
    let expected_workspace = workspaces_root.join(&workspace.id);
    if workspace_path != expected_workspace {
        bail!(
            "workspace '{}' path {} does not match expected path {}",
            workspace.id,
            workspace_path.display(),
            expected_workspace.display()
        );
    }
    match fs::symlink_metadata(&workspaces_root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "sandbox workspace root {} is a symlink",
                workspaces_root.display()
            );
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect {}", workspaces_root.display()))
        }
    }

    match fs::symlink_metadata(&workspace_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("workspace path {} is a symlink", workspace_path.display());
        }
        Ok(_) => {
            let canonical = crate::fsutil::ensure_path_within(
                &workspaces_root,
                &workspace_path,
                "workspace directory",
            )?;
            if canonical != expected_workspace {
                bail!(
                    "workspace path {} resolves unexpectedly",
                    workspace_path.display()
                );
            }
            Ok(Some(canonical))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("failed to inspect {}", workspace_path.display()))
        }
    }
}

/// Remove the workspace directory once more, after its registry record is gone.
///
/// A destroy removes the files and then the record, and the two are not one step: the
/// host work runs outside the registry lock, and the record removal is the last thing
/// that holds it. An operation that had already resolved this workspace before the
/// destroy began can therefore persist its own metadata while the destroy is releasing
/// host state, which recreates the directory after the removal above. Once the record
/// is gone nothing can resolve the workspace again, so no further write can arrive and
/// this is the last removal a destroy has to make.
///
/// The path is resolved through the same checks the first removal used, so a
/// concurrent write cannot turn this into a removal of something else.
pub(crate) fn remove_workspace_directory_after_record_removal(
    sandbox: &SandboxMetadata,
    workspace: &WorkspaceMetadata,
) -> Result<bool> {
    let Some(workspace_path) = workspace_dir_for_cleanup(sandbox, workspace)? else {
        return Ok(false);
    };
    if !workspace_path.exists() {
        return Ok(false);
    }
    remove_path_if_present(&workspace_path)?;
    if workspace_path.exists() {
        bail!(
            "workspace directory {} was recreated while the destroy ran and could not be removed",
            workspace_path.display()
        );
    }
    Ok(true)
}

fn remove_path_if_present(path: &std::path::Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)
                .with_context(|| format!("failed to remove directory {}", path.display()))?;
        }
        Ok(_) => {
            fs::remove_file(path)
                .with_context(|| format!("failed to remove file {}", path.display()))?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| format!("failed to inspect {}", path.display()))
        }
    }
    Ok(())
}
