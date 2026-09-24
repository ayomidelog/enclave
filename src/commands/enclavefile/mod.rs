//! The Enclavefile-driven lifecycle: init, up, down, and restart.
//!
//! These are the commands a project uses day to day. They are compositions of
//! daemon requests rather than daemon operations themselves, so each one decides
//! what to do from the daemon's reported state instead of assuming its own.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::cli::{RestartArgs, UpArgs};
use crate::enclavefile::{self, Enclavefile, ENCLAVEFILE_NAME};
use crate::sandbox::{SandboxListItem, SandboxStatus, DEFAULT_DEBIAN_MIRROR};

use super::{daemon, print_operation_id, send, send_managed};

mod rollback;
mod sandbox;
mod setup;
mod workspaces;

use rollback::{failed_up_rollback_target, rollback_failed_up};
use sandbox::{
    create_and_setup_sandbox, destroy_sandbox, reconcile_sandbox_definition,
    sandbox_exists_by_name, sandbox_status_by_name, start_sandbox_if_stopped, teardown_sandbox,
};
use setup::run_setup_commands;
use workspaces::bring_up_workspaces;

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
    print_operation_id();
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
    print_operation_id();
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
    print_operation_id();
    Ok(())
}
