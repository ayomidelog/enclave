use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, bail, Context, Result};

use super::runtime_limits::{legacy_workspace_cgroup_name, workspace_cgroup_name};
use crate::network;
use crate::sandbox::SandboxMetadata;
use crate::workspace::session;

use super::types::WorkspaceMetadata;

pub(super) struct WorkspaceStopCleanup {
    pub(super) sandbox: SandboxMetadata,
    pub(super) workspace: WorkspaceMetadata,
}

pub(super) fn cleanup_workspace_artifacts(
    sandbox: &SandboxMetadata,
    workspace: &WorkspaceMetadata,
) -> Result<()> {
    let workspace_path = workspace_dir_for_cleanup(sandbox, workspace)?;
    let mut errors = Vec::new();

    if let Some(pid) = workspace.runtime_pid {
        if let Err(err) = session::stop_session(pid, workspace.runtime_starttime_ticks) {
            bail!(
                "workspace '{}' runtime pid {} did not produce a successful stop result; retaining workspace files and registry record: {err:#}",
                workspace.id,
                pid,
            );
        }
        if session::process_matches(pid, workspace.runtime_starttime_ticks) {
            bail!(
                "workspace '{}' runtime pid {} is still alive after stop; retaining workspace files and registry record",
                workspace.id,
                pid
            );
        }
        if let Err(err) = remove_workspace_cgroups(sandbox, &workspace.id, pid) {
            errors.push(format!(
                "remove workspace cgroups for runtime {}: {err:#}",
                pid
            ));
        }
    }
    if let Some(ip) = workspace.assigned_ip.as_deref() {
        let report = network::teardown_workspace_network(ip, &workspace.id);
        if !report.is_complete() {
            errors.extend(report.failures.into_iter().map(|failure| {
                format!("network {} cleanup: {}", failure.resource, failure.message)
            }));
        }
    }

    if let Err(err) = crate::workspace::ensure_workspace_storage_unmounted(workspace) {
        errors.push(format!("unmount workspace storage: {err:#}"));
    }

    if !errors.is_empty() {
        bail!(
            "workspace '{}' cleanup incomplete: {}",
            workspace.id,
            errors.join("; ")
        );
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
    Ok(())
}

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

pub(super) fn run_workspace_stop_cleanups(
    cleanups: Vec<WorkspaceStopCleanup>,
    network_already_cleaned: bool,
) -> Vec<String> {
    if cleanups.is_empty() {
        return Vec::new();
    }

    let network_targets = cleanups
        .iter()
        .filter_map(|cleanup| {
            cleanup
                .workspace
                .assigned_ip
                .clone()
                .map(|ip| (ip, cleanup.workspace.id.clone()))
        })
        .collect::<Vec<_>>();
    let network_reports = if network_already_cleaned {
        Vec::new()
    } else {
        crate::network::teardown_workspace_networks(&network_targets)
    };
    let network_failures = Arc::new(
        network_reports
            .into_iter()
            .filter(|report| !report.is_complete())
            .map(|report| {
                (
                    report.workspace_id.clone(),
                    format_network_cleanup_error(&report),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>(),
    );
    // `/tmp` lives inside the workspace disk image, so clear it while the
    // image is still mounted. Doing this after the batch unmount would only
    // empty the host mountpoint and silently leave the image untouched.
    let tmp_reset_failures = Arc::new(
        cleanups
            .iter()
            .filter(|cleanup| cleanup.workspace.clear_tmp_on_restart)
            .filter_map(|cleanup| {
                crate::workspace::reset_workspace_tmp(&cleanup.workspace)
                    .err()
                    .map(|error| {
                        (
                            cleanup.workspace.id.clone(),
                            format!("failed to reset workspace /tmp: {error:#}"),
                        )
                    })
            })
            .collect::<std::collections::BTreeMap<_, _>>(),
    );
    let storage_workspaces = cleanups
        .iter()
        .map(|cleanup| cleanup.workspace.clone())
        .collect::<Vec<_>>();
    // A successful batch unmount already used one consistent mountinfo
    // snapshot. Avoid re-reading mountinfo once per workspace; retain the
    // per-workspace path as a retry fallback when the batch operation fails.
    let storage_batch_succeeded =
        match crate::workspace::ensure_workspace_storage_unmounted_many(&storage_workspaces) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!("batch workspace storage cleanup failed: {err:#}");
                false
            }
        };

    let worker_count = cleanup_worker_count(cleanups.len());
    let queue = Arc::new(Mutex::new(VecDeque::from(cleanups)));
    let (result_sender, result_receiver) = std::sync::mpsc::channel();
    let handles = (0..worker_count)
        .map(|worker_id| {
            let queue = Arc::clone(&queue);
            let network_failures = Arc::clone(&network_failures);
            let tmp_reset_failures = Arc::clone(&tmp_reset_failures);
            let result_sender = result_sender.clone();
            thread::Builder::new()
                .name(format!("enclave-workspace-cleanup-{worker_id}"))
                .spawn(move || loop {
                    let cleanup = match queue.lock() {
                        Ok(mut queue) => queue.pop_front(),
                        Err(_) => return,
                    };
                    let Some(cleanup) = cleanup else {
                        return;
                    };
                    let workspace_id = cleanup.workspace.id.clone();
                    let mut pre_failures = Vec::new();
                    if let Some(network_error) = network_failures.get(&workspace_id) {
                        pre_failures.push(network_error.clone());
                    }
                    if let Some(tmp_error) = tmp_reset_failures.get(&workspace_id) {
                        pre_failures.push(tmp_error.clone());
                    }
                    if !pre_failures.is_empty() {
                        let _ = result_sender
                            .send((workspace_id, Err(anyhow!("{}", pre_failures.join("; ")))));
                        continue;
                    }
                    let result = run_workspace_stop_cleanup(
                        cleanup,
                        network_already_cleaned,
                        storage_batch_succeeded,
                    );
                    let _ = result_sender.send((workspace_id, result));
                })
                .expect("failed to spawn workspace cleanup worker")
        })
        .collect::<Vec<_>>();

    for handle in handles {
        if let Err(err) = handle.join() {
            tracing::warn!("workspace cleanup thread panicked: {:?}", err);
        }
    }
    drop(result_sender);
    result_receiver
        .into_iter()
        .filter_map(|(workspace_id, result)| {
            result
                .err()
                .map(|error| format!("{workspace_id}: {error:#}"))
        })
        .collect()
}

pub(super) fn format_network_cleanup_error(report: &network::NetworkCleanupReport) -> String {
    format!(
        "{}: network cleanup incomplete ({})",
        report.workspace_id,
        report
            .failures
            .iter()
            .map(|failure| format!("{}: {}", failure.resource, failure.message))
            .collect::<Vec<_>>()
            .join("; ")
    )
}

fn cleanup_worker_count(job_count: usize) -> usize {
    std::env::var("ENCLAVE_CLEANUP_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=64).contains(value))
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|parallelism| parallelism.get().clamp(1, 4))
                .unwrap_or(4)
        })
        .min(job_count)
}

pub(super) fn run_workspace_stop_cleanup(
    cleanup: WorkspaceStopCleanup,
    network_already_cleaned: bool,
    storage_already_unmounted: bool,
) -> Result<()> {
    let workspace = cleanup.workspace;
    let sandbox = cleanup.sandbox;

    if let Some(pid) = workspace.runtime_pid {
        remove_workspace_cgroups(&sandbox, &workspace.id, pid)?;
    }

    if !network_already_cleaned {
        if let Some(ip) = workspace.assigned_ip.as_deref() {
            let report = crate::network::teardown_workspace_network(ip, &workspace.id);
            if !report.is_complete() {
                bail!("{}", format_network_cleanup_error(&report));
            }
        }
    }

    if storage_already_unmounted {
        return Ok(());
    }

    // `/tmp` is backed by the workspace disk image, so it must be cleared
    // before that image is unmounted.
    if workspace.clear_tmp_on_restart {
        crate::workspace::reset_workspace_tmp(&workspace)
            .with_context(|| format!("failed to reset workspace /tmp for {}", workspace.id))?;
    }
    crate::workspace::ensure_workspace_storage_unmounted(&workspace)
        .context("failed to unmount workspace storage")?;
    Ok(())
}

pub(super) fn remove_workspace_cgroups(
    sandbox: &SandboxMetadata,
    workspace_id: &str,
    pid: u32,
) -> Result<()> {
    let workspace_name = workspace_cgroup_name(&sandbox.id, workspace_id);
    let legacy_name = legacy_workspace_cgroup_name(pid);
    let sandbox_path = std::path::PathBuf::from("/sys/fs/cgroup")
        .join(crate::sandbox::cgroup::sandbox_cgroup_name(&sandbox.id))
        .join(&workspace_name);
    let mut errors = Vec::new();
    for (label, result) in [
        (
            sandbox_path.display().to_string(),
            crate::sandbox::cgroup::remove_cgroup_path(&sandbox_path),
        ),
        (
            format!("legacy {legacy_name}"),
            crate::sandbox::cgroup::remove_workspace_cgroup(&legacy_name),
        ),
    ] {
        if let Err(error) = result {
            errors.push(format!("{label}: {error:#}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("workspace cgroup cleanup incomplete: {}", errors.join("; "))
    }
}
