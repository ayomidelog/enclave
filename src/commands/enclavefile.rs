use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{bail, Context, Result};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::cli::{RestartArgs, UpArgs};
use crate::enclavefile::{self, Enclavefile, ENCLAVEFILE_NAME};
use crate::sandbox::{SandboxListItem, SandboxStatus, DEFAULT_DEBIAN_MIRROR};

use super::{daemon, send, send_managed};

pub(crate) fn run_init() -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine current directory")?;
    let path = cwd.join(ENCLAVEFILE_NAME);
    if path.exists() {
        bail!("Enclavefile already exists at {}", path.display());
    }
    let content = enclavefile::scaffold_enclavefile();
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write {}", path.display()))?;
    println!("created {}", path.display());
    Ok(())
}

pub(crate) fn run_up(socket: &Path, args: UpArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine current directory")?;
    let ef_path = enclavefile::find_enclavefile(&cwd).ok_or_else(|| {
        anyhow::anyhow!(
            "no Enclavefile found in {}. Run `enclave init` to create one.",
            cwd.display()
        )
    })?;
    let ef = enclavefile::load_enclavefile(&ef_path)?;

    daemon::ensure_daemon_running(socket)?;

    let initial_status = sandbox_status_by_name(socket, &ef.sandbox.name)?;
    let operation = (|| {
        if args.rebuild {
            if initial_status.is_some() {
                tracing::info!("rebuilding sandbox '{}'...", ef.sandbox.name);
                teardown_sandbox(socket, &ef.sandbox.name)?;
                destroy_sandbox(socket, &ef.sandbox.name)?;
            }
            create_and_setup_sandbox(socket, &ef, args.cache_setup)?;
        } else if initial_status.is_some() {
            tracing::info!("sandbox '{}' already exists, starting...", ef.sandbox.name);
            start_sandbox_if_stopped(socket, &ef.sandbox.name)?;
            reconcile_sandbox_definition(socket, &ef)?;
            run_setup_commands(socket, &ef, args.cache_setup)?;
        } else {
            create_and_setup_sandbox(socket, &ef, args.cache_setup)?;
        }
        bring_up_workspaces(socket, &ef, &ef_path)
    })();
    if let Err(error) = operation {
        let target = failed_up_rollback_target(initial_status.as_ref(), args.rebuild);
        return Err(rollback_failed_up(socket, &ef.sandbox.name, target, error));
    }

    println!("environment is up");
    Ok(())
}

pub(crate) fn run_down(socket: &Path) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine current directory")?;
    let ef_path = enclavefile::find_enclavefile(&cwd).ok_or_else(|| {
        anyhow::anyhow!(
            "no Enclavefile found in {}. Run `enclave init` to create one.",
            cwd.display()
        )
    })?;
    let ef = enclavefile::load_enclavefile(&ef_path)?;

    daemon::ensure_daemon_running(socket)?;

    if !sandbox_exists_by_name(socket, &ef.sandbox.name)? {
        println!(
            "sandbox '{}' does not exist, nothing to stop",
            ef.sandbox.name
        );
        return Ok(());
    }

    teardown_sandbox(socket, &ef.sandbox.name)?;
    println!("environment is down");
    Ok(())
}

pub(crate) fn run_restart(socket: &Path, args: RestartArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to determine current directory")?;
    let ef_path = enclavefile::find_enclavefile(&cwd).ok_or_else(|| {
        anyhow::anyhow!(
            "no Enclavefile found in {}. Run `enclave init` to create one.",
            cwd.display()
        )
    })?;
    let ef = enclavefile::load_enclavefile(&ef_path)?;

    daemon::ensure_daemon_running(socket)?;

    let sandbox_exists = sandbox_status_by_name(socket, &ef.sandbox.name)?.is_some();
    let operation = (|| {
        if args.rebuild {
            if sandbox_exists {
                tracing::info!("rebuilding sandbox '{}'...", ef.sandbox.name);
                teardown_sandbox(socket, &ef.sandbox.name)?;
                destroy_sandbox(socket, &ef.sandbox.name)?;
            }
            create_and_setup_sandbox(socket, &ef, args.cache_setup)?;
        } else if sandbox_exists {
            teardown_sandbox(socket, &ef.sandbox.name)?;
            start_sandbox_if_stopped(socket, &ef.sandbox.name)?;
            reconcile_sandbox_definition(socket, &ef)?;
            run_setup_commands(socket, &ef, args.cache_setup)?;
        } else {
            create_and_setup_sandbox(socket, &ef, args.cache_setup)?;
        }
        bring_up_workspaces(socket, &ef, &ef_path)
    })();
    if let Err(error) = operation {
        return Err(rollback_failed_up(
            socket,
            &ef.sandbox.name,
            Some(SandboxStatus::Stopped),
            error,
        ));
    }

    println!("environment restarted");
    Ok(())
}

fn sandbox_exists_by_name(socket: &Path, name: &str) -> Result<bool> {
    Ok(sandbox_status_by_name(socket, name)?.is_some())
}

fn sandbox_status_by_name(socket: &Path, name: &str) -> Result<Option<SandboxStatus>> {
    let response = send(socket, "sandbox.list", json!({}))?;
    let sandboxes: Vec<SandboxListItem> = serde_json::from_value(response)?;
    Ok(sandboxes
        .into_iter()
        .find(|sandbox| sandbox.name == name)
        .map(|sandbox| sandbox.status))
}

fn failed_up_rollback_target(
    initial_status: Option<&SandboxStatus>,
    rebuilding: bool,
) -> Option<SandboxStatus> {
    if rebuilding || initial_status.is_none() {
        return Some(SandboxStatus::Stopped);
    }
    match initial_status.expect("checked above") {
        SandboxStatus::Running => None,
        SandboxStatus::Paused => Some(SandboxStatus::Paused),
        SandboxStatus::Stopped => Some(SandboxStatus::Stopped),
    }
}

fn rollback_failed_up(
    socket: &Path,
    sandbox_name: &str,
    target_status: Option<SandboxStatus>,
    cause: anyhow::Error,
) -> anyhow::Error {
    let Some(target_status) = target_status else {
        return cause;
    };
    let current_status = match sandbox_status_by_name(socket, sandbox_name) {
        Ok(Some(status)) => status,
        Ok(None) => return cause,
        Err(error) => {
            return anyhow::anyhow!(
                "environment setup failed: {cause:#}; could not inspect sandbox '{}' for rollback: {error:#}",
                sandbox_name
            );
        }
    };
    if current_status == target_status {
        return cause;
    }

    let (action, description) = match target_status {
        SandboxStatus::Stopped => ("sandbox.stop", "stop"),
        SandboxStatus::Paused => ("sandbox.pause", "pause"),
        SandboxStatus::Running => return cause,
    };
    if let Err(error) = send(socket, action, json!({ "sandbox": sandbox_name })) {
        return anyhow::anyhow!(
            "environment setup failed: {cause:#}; sandbox '{}' remains {:?} because rollback could not {description} it: {error:#}",
            sandbox_name,
            current_status
        );
    }
    cause.context(format!(
        "sandbox '{}' was rolled back to {:?} after setup failed",
        sandbox_name, target_status
    ))
}

fn start_sandbox_if_stopped(socket: &Path, name: &str) -> Result<()> {
    match send(socket, "sandbox.start", json!({ "sandbox": name })) {
        Ok(_) => Ok(()),
        Err(error) => {
            if sandbox_status_by_name(socket, name)?
                .is_some_and(|status| status == SandboxStatus::Running)
            {
                return Ok(());
            }
            Err(error)
        }
    }
}

fn create_and_setup_sandbox(socket: &Path, ef: &Enclavefile, cache_setup: bool) -> Result<()> {
    println!(
        "creating sandbox '{}' with suite '{}' (this may take several minutes)...",
        ef.sandbox.name, ef.sandbox.suite
    );
    let request = json!({
        "name": ef.sandbox.name,
        "suite": ef.sandbox.suite,
        "mirror": DEFAULT_DEBIAN_MIRROR,
        "bootstrap_method": ef.sandbox.bootstrap_method.to_string(),
        "memory_mb": ef.sandbox.memory_mb,
        "cpu_percent": ef.sandbox.cpu_percent,
        "max_procs": ef.sandbox.max_procs,
    });
    send(socket, "sandbox.create", request)?;

    run_setup_commands(socket, ef, cache_setup)?;

    Ok(())
}

fn reconcile_sandbox_definition(socket: &Path, ef: &Enclavefile) -> Result<()> {
    send_managed(
        socket,
        "sandbox.update",
        json!({
            "sandbox": ef.sandbox.name,
            "memory_mb": ef.sandbox.memory_mb,
            "cpu_percent": ef.sandbox.cpu_percent,
            "max_procs": ef.sandbox.max_procs,
        }),
    )?;
    Ok(())
}

fn run_setup_commands(socket: &Path, ef: &Enclavefile, cache_setup: bool) -> Result<()> {
    if ef.sandbox.setup.is_empty() {
        return Ok(());
    }
    tracing::info!("running setup commands...");
    let setup_digest = setup_digest(ef);
    for (i, cmd) in ef.sandbox.setup.iter().enumerate() {
        tracing::info!("  [{}/{}] {}", i + 1, ef.sandbox.setup.len(), cmd);
        let result = send(
            socket,
            "sandbox.exec_setup",
            json!({
                "sandbox": ef.sandbox.name,
                "command": cmd,
                "cache_setup": cache_setup,
                "setup_digest": setup_digest,
                "setup_index": i,
            }),
        );
        if let Err(err) = result {
            bail!("setup command failed: {}\n  command: {}", err, cmd);
        }
    }

    Ok(())
}

fn setup_digest(ef: &Enclavefile) -> String {
    let mut digest = Sha256::new();
    digest.update(ef.sandbox.name.as_bytes());
    digest.update([0]);
    digest.update(ef.sandbox.suite.as_bytes());
    digest.update([0]);
    digest.update(ef.sandbox.bootstrap_method.to_string().as_bytes());
    for command in &ef.sandbox.setup {
        digest.update([0xff]);
        digest.update(command.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn teardown_sandbox(socket: &Path, name: &str) -> Result<()> {
    tracing::info!("stopping sandbox '{}'...", name);
    match send(socket, "sandbox.stop", json!({ "sandbox": name })) {
        Ok(_) => Ok(()),
        Err(error) => match sandbox_status_by_name(socket, name)? {
            None | Some(SandboxStatus::Stopped) => Ok(()),
            Some(_) => Err(error),
        },
    }
}

fn destroy_sandbox(socket: &Path, name: &str) -> Result<()> {
    tracing::info!("destroying sandbox '{}'...", name);
    match send(socket, "sandbox.destroy", json!({ "sandbox": name })) {
        Ok(_) => Ok(()),
        Err(error) => {
            if sandbox_status_by_name(socket, name)?.is_none() {
                return Ok(());
            }
            Err(error)
        }
    }
}

fn bring_up_workspaces(socket: &Path, ef: &Enclavefile, ef_path: &Path) -> Result<()> {
    let definitions = ef
        .workspace
        .iter()
        .enumerate()
        .map(|(index, (key, workspace))| {
            let workspace_dir = match (
                workspace.workspace_dir.as_deref(),
                workspace.path.as_deref(),
            ) {
                (Some(raw), _) => Some(
                    crate::enclavefile::resolve_workspace_host_dir(ef_path, raw, "workspace_dir")
                        .with_context(|| format!("failed to resolve workspace '{}'", key))?,
                ),
                (None, Some(raw)) => Some(
                    crate::enclavefile::resolve_workspace_host_dir(ef_path, raw, "path")
                        .with_context(|| format!("failed to resolve workspace '{}'", key))?,
                ),
                (None, None) => None,
            };
            Ok::<_, anyhow::Error>((index, key.as_str(), workspace, workspace_dir))
        })
        .collect::<Result<Vec<_>>>()?;

    start_workspace_definitions_bulk(socket, &ef.sandbox.name, &definitions)?;

    Ok(())
}

#[allow(dead_code)]
fn start_workspace_definitions(
    socket: &Path,
    sandbox_name: &str,
    definitions: &[(
        usize,
        &str,
        &crate::enclavefile::WorkspaceSection,
        Option<String>,
    )],
) -> Result<()> {
    if definitions.is_empty() {
        return Ok(());
    }

    let worker_count = std::env::var("ENCLAVE_UP_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=64).contains(value))
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|parallelism| parallelism.get().saturating_mul(2).max(1))
                .unwrap_or(4)
        })
        .min(definitions.len());
    let queue = Arc::new(Mutex::new(VecDeque::from_iter(0..definitions.len())));
    let (result_sender, result_receiver) = std::sync::mpsc::channel();

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = Arc::clone(&queue);
            let result_sender = result_sender.clone();
            scope.spawn(move || loop {
                let job_index = match queue.lock() {
                    Ok(mut queue) => queue.pop_front(),
                    Err(_) => return,
                };
                let Some(job_index) = job_index else {
                    return;
                };
                let (_, key, workspace, workspace_dir) = &definitions[job_index];
                let result = ensure_workspace_started(
                    socket,
                    sandbox_name,
                    key,
                    workspace,
                    workspace_dir.as_deref(),
                );
                let _ = result_sender.send((job_index, result));
            });
        }
        drop(result_sender);

        let mut results = (0..definitions.len())
            .map(|_| None)
            .collect::<Vec<Option<Result<()>>>>();
        for (index, result) in result_receiver {
            results[index] = Some(result);
        }
        for (index, result) in results.into_iter().enumerate() {
            if let Some(Err(error)) = result {
                return Err(error).with_context(|| {
                    format!("failed to start workspace '{}'", definitions[index].1)
                });
            }
        }
        Ok(())
    })
}

fn start_workspace_definitions_bulk(
    socket: &Path,
    sandbox_name: &str,
    definitions: &[(
        usize,
        &str,
        &crate::enclavefile::WorkspaceSection,
        Option<String>,
    )],
) -> Result<()> {
    if definitions.is_empty() {
        return Ok(());
    }
    let workspaces = definitions
        .iter()
        .map(|(_, _, workspace, workspace_dir)| {
            json!({
                "sandbox_id": sandbox_name,
                "name": workspace.name,
                "path": workspace_dir,
                "cpu_seconds": workspace.cpu_seconds,
                "cpu_percent": workspace.cpu_percent,
                "memory_mb": workspace.memory_mb,
                "max_procs": workspace.max_procs,
                "max_open_files": workspace.max_open_files,
                "disk_mb": workspace.disk_mb,
                "clear_tmp_on_restart": workspace.clear_tmp_on_restart,
                "auth": workspace.auth,
                "env_tokens": workspace.env_tokens,
                "ports": workspace.ports,
                "run": workspace.run,
            })
        })
        .collect::<Vec<_>>();
    let result = send_managed(
        socket,
        "workspace.start_many",
        json!({ "workspaces": workspaces }),
    )?;
    let failures = result
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| {
            item.get("ok")
                .and_then(|value| value.as_bool())
                .is_some_and(|ok| !ok)
        })
        .map(|item| {
            item.get("error")
                .and_then(|error| error.as_str())
                .unwrap_or("workspace startup failed")
                .to_string()
        })
        .collect::<Vec<_>>();
    if !failures.is_empty() {
        bail!(
            "one or more workspaces failed to start: {}",
            failures.join("; ")
        );
    }
    Ok(())
}

#[allow(dead_code)]
fn ensure_workspace_started(
    socket: &Path,
    sandbox_name: &str,
    key: &str,
    workspace: &crate::enclavefile::WorkspaceSection,
    workspace_dir: Option<&str>,
) -> Result<()> {
    tracing::info!("creating workspace '{}'...", workspace.name);
    let create_result = send_managed(
        socket,
        "workspace.create",
        json!({
            "sandbox_id": sandbox_name,
            "name": workspace.name,
            "path": workspace_dir,
            "cpu_seconds": workspace.cpu_seconds,
            "cpu_percent": workspace.cpu_percent,
            "memory_mb": workspace.memory_mb,
            "max_procs": workspace.max_procs,
            "max_open_files": workspace.max_open_files,
            "disk_mb": workspace.disk_mb,
            "clear_tmp_on_restart": workspace.clear_tmp_on_restart,
            "auth": workspace.auth.clone(),
            "env_tokens": workspace.env_tokens.clone(),
            "ports": workspace.ports.clone(),
        }),
    );
    match create_result {
        Ok(_) => Ok(()),
        Err(error) if format!("{error:#}").contains("already exists") => {
            tracing::info!(
                "  workspace '{}' already exists, starting...",
                workspace.name
            );
            if let Err(update_error) = send_managed(
                socket,
                "workspace.update",
                json!({
                    "sandbox": sandbox_name,
                    "workspace": workspace.name,
                    "cpu_seconds": workspace.cpu_seconds,
                    "cpu_percent": workspace.cpu_percent,
                    "memory_mb": workspace.memory_mb,
                    "max_procs": workspace.max_procs,
                    "max_open_files": workspace.max_open_files,
                    "disk_mb": workspace.disk_mb,
                    "clear_tmp_on_restart": workspace.clear_tmp_on_restart,
                    "auth": workspace.auth.clone(),
                    "env_tokens": workspace.env_tokens.clone(),
                }),
            ) {
                tracing::warn!(
                    "failed to update auth providers for workspace '{}': {update_error:#}",
                    key
                );
            }
            send_managed(
                socket,
                "workspace.start",
                json!({
                    "sandbox": sandbox_name,
                    "workspace": workspace.name,
                }),
            )
            .map(|_| ())
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::failed_up_rollback_target;
    use crate::sandbox::SandboxStatus;

    #[test]
    fn failed_up_rolls_back_new_and_stopped_sandboxes() {
        assert_eq!(
            failed_up_rollback_target(None, false),
            Some(SandboxStatus::Stopped)
        );
        assert_eq!(
            failed_up_rollback_target(Some(&SandboxStatus::Stopped), false),
            Some(SandboxStatus::Stopped)
        );
    }

    #[test]
    fn failed_up_restores_paused_sandboxes_and_preserves_running_ones() {
        assert_eq!(
            failed_up_rollback_target(Some(&SandboxStatus::Paused), false),
            Some(SandboxStatus::Paused)
        );
        assert_eq!(
            failed_up_rollback_target(Some(&SandboxStatus::Running), false),
            None
        );
    }

    #[test]
    fn failed_rebuild_rolls_back_to_stopped() {
        assert_eq!(
            failed_up_rollback_target(Some(&SandboxStatus::Running), true),
            Some(SandboxStatus::Stopped)
        );
    }
}
