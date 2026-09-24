use super::*;

use crate::hostcmd::HostCommand;

// A setup command is arbitrary user work: a package install or a build. It gets
// a generous deadline so ordinary setup is never cut short, but not an unbounded
// one, so a hung command cannot hold the sandbox forever.
const SETUP_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

pub fn exec_setup_command(
    state_dir: &Path,
    selector: &str,
    command: &str,
    cache_setup: bool,
    setup_digest: Option<&str>,
    setup_index: Option<u64>,
) -> Result<serde_json::Value> {
    use crate::registry::with_registry;

    let rootfs_path = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let entry = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;
        let mut metadata = entry.metadata.clone();
        normalize_sandbox_metadata(&mut metadata);
        Ok(crate::sandbox::effective_rootfs_path(&metadata))
    })?;

    let cache_marker = if cache_setup {
        let digest = setup_digest.ok_or_else(|| anyhow!("cached setup requires setup_digest"))?;
        let index = setup_index.ok_or_else(|| anyhow!("cached setup requires setup_index"))?;
        if setup_cache::is_complete(Path::new(&rootfs_path), digest, index)? {
            return Ok(serde_json::json!({"cached": true, "exit_code": 0}));
        }
        Some((digest.to_string(), index))
    } else {
        None
    };

    let output = HostCommand::new("chroot")
        .arg(&rootfs_path)
        .arg("/bin/sh")
        .arg("-c")
        .arg(command)
        .timeout(SETUP_COMMAND_TIMEOUT)
        .run()
        .map_err(|error| setup_command_error(error, command))?;

    let exit_code = output.status.code().unwrap_or(1);
    let stdout = output.stdout_text();
    let stderr = output.stderr_text();

    if !output.success() {
        bail!(
            "setup command exited with status {}: {}{}",
            exit_code,
            stderr,
            if stderr.is_empty() {
                stdout.trim().to_string()
            } else {
                String::new()
            }
        );
    }

    if let Some((digest, index)) = cache_marker {
        setup_cache::mark_complete(Path::new(&rootfs_path), &digest, index)?;
    }

    Ok(serde_json::json!({
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
    }))
}

/// Explain a setup-command failure, naming a deadline that expired as such.
///
/// A timeout is not the same as a command that failed, and an operator needs to
/// know which one happened and how to change it.
fn setup_command_error(error: anyhow::Error, command: &str) -> anyhow::Error {
    if crate::hostcmd::is_timeout(&error) {
        return error.context(format!(
            "setup command was stopped after {SETUP_COMMAND_TIMEOUT:?}; raise ENCLAVE_HOST_COMMAND_TIMEOUT_SECS if it needs longer: {command}"
        ));
    }
    error.context(format!(
        "failed to execute setup command in sandbox: {command}"
    ))
}
