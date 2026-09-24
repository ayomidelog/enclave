use super::*;

pub(crate) fn session_helper_path(workspace: &WorkspaceMetadata) -> PathBuf {
    sandbox_runtime_dir(workspace).join(SESSION_HELPER_BASENAME)
}

pub(crate) fn prepare_session_helper(workspace: &WorkspaceMetadata) -> Result<PathBuf> {
    let _guard = SESSION_HELPER_PREPARE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| anyhow::anyhow!("session helper preparation lock poisoned"))?;
    let helper_path = session_helper_path(workspace);
    let source_exe = resolve_session_helper_source();
    // Replacing an already-running executable can fail with ETXTBSY on Linux.
    // Reuse the sandbox-local helper for the lifetime of the sandbox.
    if helper_path.is_file() {
        return Ok(helper_path);
    }
    if let Some(parent) = helper_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let temp_path = helper_path.with_extension("tmp");
    if temp_path.exists() {
        fs::remove_file(&temp_path)
            .with_context(|| format!("failed to remove stale {}", temp_path.display()))?;
    }
    fs::copy(&source_exe, &temp_path).with_context(|| {
        format!(
            "failed to copy session helper from {} to {}",
            source_exe.display(),
            temp_path.display()
        )
    })?;
    let mut permissions = fs::metadata(&temp_path)
        .with_context(|| format!("failed to stat {}", temp_path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&temp_path, permissions)
        .with_context(|| format!("failed to chmod {}", temp_path.display()))?;
    match fs::hard_link(&temp_path, &helper_path) {
        Ok(()) => {
            fs::remove_file(&temp_path).with_context(|| {
                format!(
                    "failed to remove temporary session helper {}",
                    temp_path.display()
                )
            })?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // Another starter won the create race. Never replace a helper that
            // may already be executing; the sandbox helper is immutable for
            // the lifetime of the sandbox.
            let _ = fs::remove_file(&temp_path);
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to install session helper {}", helper_path.display())
            });
        }
    }
    Ok(helper_path)
}

pub(crate) fn resolve_session_helper_source() -> PathBuf {
    for candidate in [HELPER_OVERRIDE_ENV, TEST_BINARY_ENV] {
        if let Some(path) = std::env::var_os(candidate).map(PathBuf::from) {
            if path.is_file() {
                return path;
            }
        }
    }
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(inferred) = infer_workspace_helper_from_current_exe(&current_exe) {
            return inferred;
        }
    }
    PathBuf::from(SELF_EXE_PATH)
}

pub(crate) fn infer_workspace_helper_from_current_exe(current_exe: &Path) -> Option<PathBuf> {
    let deps_dir = current_exe.parent()?;
    if deps_dir.file_name()? != "deps" {
        return None;
    }
    let profile_dir = deps_dir.parent()?;
    let candidate = profile_dir.join("enclave");
    candidate.is_file().then_some(candidate)
}
