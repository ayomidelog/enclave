use super::*;

pub(crate) fn ensure_transfer_success(label: &str, output: &Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "{} failed (exit {}): {}",
        label,
        output
            .status
            .code()
            .map_or_else(|| "signal".to_string(), |code| code.to_string()),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

pub(crate) fn run_workspace_utility(
    workspace: &WorkspaceMetadata,
    command: &[&str],
    client_stream: Option<&UnixStream>,
) -> Result<Output> {
    let key = UtilityCacheKey {
        sandbox_id: workspace.sandbox_id.clone(),
        workspace_id: workspace.id.clone(),
        runtime_pid: workspace.runtime_pid.unwrap_or_default(),
        runtime_starttime_ticks: workspace.runtime_starttime_ticks.unwrap_or_default(),
        command: command.first().copied().unwrap_or_default().to_string(),
    };
    if utility_uses_busybox(&key) {
        return run_busybox_utility(workspace, command, client_stream);
    }

    let primary_command = command
        .iter()
        .map(|item| (*item).to_string())
        .collect::<Vec<_>>();
    let primary = spawn_workspace_command(
        workspace,
        "/home",
        &primary_command,
        Stdio::null(),
        Stdio::null(),
        Stdio::piped(),
    )?;
    let mut primary = ChildGuard::new(primary);
    let output = wait_child_output(&mut primary, client_stream)?;
    if output.status.code() != Some(127) {
        cache_utility_choice(key, false);
        return Ok(output);
    }

    cache_utility_choice(key, true);
    run_busybox_utility(workspace, command, client_stream)
}

pub(crate) fn run_busybox_utility(
    workspace: &WorkspaceMetadata,
    command: &[&str],
    client_stream: Option<&UnixStream>,
) -> Result<Output> {
    let mut fallback_command = vec!["/bin/busybox".to_string()];
    fallback_command.extend(command.iter().map(|item| (*item).to_string()));
    let fallback = spawn_workspace_command(
        workspace,
        "/home",
        &fallback_command,
        Stdio::null(),
        Stdio::null(),
        Stdio::piped(),
    )?;
    let mut fallback = ChildGuard::new(fallback);
    wait_child_output(&mut fallback, client_stream)
}

pub(crate) fn utility_uses_busybox(key: &UtilityCacheKey) -> bool {
    UTILITY_CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|cache| cache.get(key).copied())
        .unwrap_or(false)
}

pub(crate) fn cache_utility_choice(key: UtilityCacheKey, uses_busybox: bool) {
    let Ok(mut cache) = UTILITY_CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    else {
        return;
    };
    if cache.len() >= UTILITY_CACHE_LIMIT && !cache.contains_key(&key) {
        if let Some(oldest) = cache.keys().next().cloned() {
            cache.remove(&oldest);
        }
    }
    cache.insert(key, uses_busybox);
}
