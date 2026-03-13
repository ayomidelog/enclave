use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::cli::{RestartArgs, UpArgs};
use crate::enclavefile::{self, Enclavefile, ENCLAVEFILE_NAME};
use crate::sandbox::{SandboxListItem, DEFAULT_DEBIAN_MIRROR};

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

    let sandbox_exists = sandbox_exists_by_name(socket, &ef.sandbox.name)?;

    if args.rebuild && sandbox_exists {
        tracing::info!("rebuilding sandbox '{}'...", ef.sandbox.name);
        teardown_sandbox(socket, &ef.sandbox.name)?;
        destroy_sandbox(socket, &ef.sandbox.name)?;
        create_and_setup_sandbox(socket, &ef)?;
    } else if sandbox_exists {
        tracing::info!("sandbox '{}' already exists, starting...", ef.sandbox.name);
        start_sandbox_if_stopped(socket, &ef.sandbox.name)?;
        reconcile_sandbox_definition(socket, &ef)?;

        run_setup_commands(socket, &ef)?;
    } else {
        create_and_setup_sandbox(socket, &ef)?;
    }

    bring_up_workspaces(socket, &ef, &ef_path)?;

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

    let sandbox_exists = sandbox_exists_by_name(socket, &ef.sandbox.name)?;

    if args.rebuild {
        if sandbox_exists {
            tracing::info!("rebuilding sandbox '{}'...", ef.sandbox.name);
            teardown_sandbox(socket, &ef.sandbox.name)?;
            destroy_sandbox(socket, &ef.sandbox.name)?;
        }
        create_and_setup_sandbox(socket, &ef)?;
    } else if sandbox_exists {
        teardown_sandbox(socket, &ef.sandbox.name)?;
        start_sandbox_if_stopped(socket, &ef.sandbox.name)?;
        reconcile_sandbox_definition(socket, &ef)?;
        run_setup_commands(socket, &ef)?;
    } else {
        create_and_setup_sandbox(socket, &ef)?;
    }

    bring_up_workspaces(socket, &ef, &ef_path)?;

    println!("environment restarted");
    Ok(())
}

fn sandbox_exists_by_name(socket: &Path, name: &str) -> Result<bool> {
    let response = send(socket, "sandbox.list", json!({}))?;
    let sandboxes: Vec<SandboxListItem> = serde_json::from_value(response)?;
    Ok(sandboxes.iter().any(|s| s.name == name))
}

fn start_sandbox_if_stopped(socket: &Path, name: &str) -> Result<()> {
    if let Err(err) = send(socket, "sandbox.start", json!({ "sandbox": name })) {
        let msg = format!("{err:#}");
        if !msg.contains("already running") {
            return Err(err);
        }
    }
    Ok(())
}

fn create_and_setup_sandbox(socket: &Path, ef: &Enclavefile) -> Result<()> {
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

    run_setup_commands(socket, ef)?;

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

fn run_setup_commands(socket: &Path, ef: &Enclavefile) -> Result<()> {
    if ef.sandbox.setup.is_empty() {
        return Ok(());
    }
    tracing::info!("running setup commands...");
    for (i, cmd) in ef.sandbox.setup.iter().enumerate() {
        tracing::info!("  [{}/{}] {}", i + 1, ef.sandbox.setup.len(), cmd);
        let result = send(
            socket,
            "sandbox.exec_setup",
            json!({
                "sandbox": ef.sandbox.name,
                "command": cmd,
            }),
        );
        if let Err(err) = result {
            bail!("setup command failed: {}\n  command: {}", err, cmd);
        }
    }

    Ok(())
}

fn teardown_sandbox(socket: &Path, name: &str) -> Result<()> {
    tracing::info!("stopping sandbox '{}'...", name);
    if let Err(err) = send(socket, "sandbox.stop", json!({ "sandbox": name })) {
        let msg = format!("{err:#}");
        if !msg.contains("already stopped") && !msg.contains("not found") {
            return Err(err);
        }
    }
    Ok(())
}

fn destroy_sandbox(socket: &Path, name: &str) -> Result<()> {
    tracing::info!("destroying sandbox '{}'...", name);
    if let Err(err) = send(socket, "sandbox.destroy", json!({ "sandbox": name })) {
        let msg = format!("{err:#}");
        if !msg.contains("not found") {
            return Err(err);
        }
    }
    Ok(())
}

fn bring_up_workspaces(socket: &Path, ef: &Enclavefile, ef_path: &Path) -> Result<()> {
    for (key, ws) in &ef.workspace {
        let workspace_dir = match (ws.workspace_dir.as_deref(), ws.path.as_deref()) {
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
        tracing::info!("creating workspace '{}'...", ws.name);
        let create_result = send_managed(
            socket,
            "workspace.create",
            json!({
                "sandbox_id": ef.sandbox.name,
                "name": ws.name,
                "path": workspace_dir,
                "cpu_seconds": ws.cpu_seconds,
                "cpu_percent": ws.cpu_percent,
                "memory_mb": ws.memory_mb,
                "max_procs": ws.max_procs,
                "max_open_files": ws.max_open_files,
                "auth": ws.auth.clone(),
                "env_tokens": ws.env_tokens.clone(),
                "ports": ws.ports.clone(),
            }),
        );
        match create_result {
            Ok(_) => {}
            Err(err) => {
                let msg = format!("{err:#}");
                if msg.contains("already exists") {
                    tracing::info!("  workspace '{}' already exists, starting...", ws.name);

                    if let Err(err) = send_managed(
                        socket,
                        "workspace.update",
                        json!({
                            "sandbox": ef.sandbox.name,
                            "workspace": ws.name,
                            "cpu_seconds": ws.cpu_seconds,
                            "cpu_percent": ws.cpu_percent,
                            "memory_mb": ws.memory_mb,
                            "max_procs": ws.max_procs,
                            "max_open_files": ws.max_open_files,
                            "auth": ws.auth.clone(),
                            "env_tokens": ws.env_tokens.clone(),
                            "ports": ws.ports.clone(),
                        }),
                    ) {
                        tracing::warn!(
                            "failed to update auth providers for workspace '{}': {err:#}",
                            key
                        );
                    }
                    send_managed(
                        socket,
                        "workspace.start",
                        json!({
                            "sandbox": ef.sandbox.name,
                            "workspace": ws.name,
                        }),
                    )
                    .with_context(|| format!("failed to start workspace '{}'", key))?;
                } else {
                    return Err(err)
                        .with_context(|| format!("failed to create workspace '{}'", key));
                }
            }
        }

        if let Some(run_cmd) = &ws.run {
            tracing::info!("  executing: {}", run_cmd);
            let exec_result = send(
                socket,
                "workspace.exec",
                json!({
                    "sandbox_id": ef.sandbox.name,
                    "workspace_id": ws.name,
                    "cwd": "/home",
                    "command": ["sh", "-c", run_cmd],
                }),
            );
            if let Err(err) = exec_result {
                tracing::warn!("  run command for workspace '{}' failed: {err:#}", key);
            }
        }
    }

    Ok(())
}
