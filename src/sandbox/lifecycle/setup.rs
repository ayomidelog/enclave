use super::*;

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

    let output = Command::new("chroot")
        .arg(&rootfs_path)
        .arg("/bin/sh")
        .arg("-c")
        .arg(command)
        .output()
        .with_context(|| format!("failed to execute setup command in sandbox: {}", command))?;

    let exit_code = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        bail!(
            "setup command exited with status {}: {}{}",
            exit_code,
            stderr.trim(),
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
