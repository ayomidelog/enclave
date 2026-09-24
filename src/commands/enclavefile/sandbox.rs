//! Sandbox-level requests for the Enclavefile lifecycle.

use super::*;

pub(super) fn sandbox_exists_by_name(socket: &Path, name: &str) -> Result<bool> {
    Ok(sandbox_status_by_name(socket, name)?.is_some())
}

pub(super) fn sandbox_status_by_name(socket: &Path, name: &str) -> Result<Option<SandboxStatus>> {
    let response = send(socket, "sandbox.list", json!({}))?;
    let sandboxes: Vec<SandboxListItem> = serde_json::from_value(response)?;
    Ok(sandboxes
        .into_iter()
        .find(|sandbox| sandbox.name == name)
        .map(|sandbox| sandbox.status))
}

pub(super) fn start_sandbox_if_stopped(socket: &Path, name: &str) -> Result<()> {
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

pub(super) fn create_and_setup_sandbox(
    socket: &Path,
    ef: &Enclavefile,
    cache_setup: bool,
) -> Result<()> {
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

pub(super) fn reconcile_sandbox_definition(socket: &Path, ef: &Enclavefile) -> Result<()> {
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

pub(super) fn teardown_sandbox(socket: &Path, name: &str) -> Result<()> {
    tracing::info!("stopping sandbox '{}'...", name);
    match send(socket, "sandbox.stop", json!({ "sandbox": name })) {
        Ok(_) => Ok(()),
        Err(error) => match sandbox_status_by_name(socket, name)? {
            None | Some(SandboxStatus::Stopped) => Ok(()),
            Some(_) => Err(error),
        },
    }
}

pub(super) fn destroy_sandbox(socket: &Path, name: &str) -> Result<()> {
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
