use super::*;

pub(super) fn require_param_str<'a>(params: &'a Value, keys: &[&str]) -> Result<&'a str> {
    for key in keys {
        if let Some(value) = params.get(key).and_then(Value::as_str) {
            return Ok(value);
        }
    }
    bail!("missing '{}'", keys[0])
}

pub(super) fn parse_string_array(params: &Value, key: &str) -> Result<Vec<String>> {
    let values = params
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("missing '{}' array", key))?;

    let mut out = Vec::with_capacity(values.len());
    for (idx, value) in values.iter().enumerate() {
        let item = value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("'{}[{}]' must be a string", key, idx))?;
        out.push(item.to_string());
    }

    Ok(out)
}

pub(super) fn parse_optional_u64_field(params: &Value, key: &str) -> Result<Option<Option<u64>>> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(Some(None));
    }
    let parsed = value
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("'{}' must be an unsigned integer", key))?;
    Ok(Some(Some(parsed)))
}

pub(super) fn parse_optional_f64_field(params: &Value, key: &str) -> Result<Option<Option<f64>>> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(Some(None));
    }
    let parsed = value
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("'{}' must be a number", key))?;
    Ok(Some(Some(parsed)))
}

pub(super) fn parse_workspace_limits_create(params: &Value) -> Result<workspace::WorkspaceLimits> {
    let cpu_seconds = params.get("cpu_seconds").and_then(Value::as_u64);
    let cpu_percent = params.get("cpu_percent").and_then(Value::as_f64);
    let memory_mb = params.get("memory_mb").and_then(Value::as_u64);
    let max_procs = params.get("max_procs").and_then(Value::as_u64);
    let max_open_files = params.get("max_open_files").and_then(Value::as_u64);
    let disk_mb = params.get("disk_mb").and_then(Value::as_u64);
    let memory_bytes = checked_megabytes(memory_mb, "memory_mb")?;
    let disk_bytes = checked_megabytes(disk_mb, "disk_mb")?;
    let limits = workspace::WorkspaceLimits {
        cpu_seconds,
        cpu_percent,
        memory_bytes,
        max_processes: max_procs,
        max_open_files,
        disk_bytes,
    };
    limits.validate()?;
    Ok(limits)
}

pub(super) fn parse_required_disk_bytes(params: &Value) -> Result<u64> {
    let disk_mb = params
        .get("disk_mb")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("missing 'disk_mb' unsigned integer"))?;
    disk_mb
        .checked_mul(1024 * 1024)
        .ok_or_else(|| anyhow::anyhow!("'disk_mb' is too large"))
}

pub(super) fn parse_workspace_limits_update(
    params: &Value,
) -> Result<workspace::WorkspaceLimitsUpdate> {
    let memory_bytes =
        checked_optional_megabytes(parse_optional_u64_field(params, "memory_mb")?, "memory_mb")?;
    let disk_bytes =
        checked_optional_megabytes(parse_optional_u64_field(params, "disk_mb")?, "disk_mb")?;
    Ok(workspace::WorkspaceLimitsUpdate {
        clear_tmp_on_restart: parse_optional_bool_field(params, "clear_tmp_on_restart")?,
        cpu_seconds: parse_optional_u64_field(params, "cpu_seconds")?,
        cpu_percent: parse_optional_f64_field(params, "cpu_percent")?,
        memory_bytes,
        max_processes: parse_optional_u64_field(params, "max_procs")?,
        max_open_files: parse_optional_u64_field(params, "max_open_files")?,
        disk_bytes,
    })
}

pub(super) fn checked_megabytes(value: Option<u64>, key: &str) -> Result<Option<u64>> {
    value
        .map(|value| {
            value
                .checked_mul(1024 * 1024)
                .ok_or_else(|| anyhow::anyhow!("'{key}' is too large"))
        })
        .transpose()
}

pub(super) fn checked_optional_megabytes(
    value: Option<Option<u64>>,
    key: &str,
) -> Result<Option<Option<u64>>> {
    value.map(|value| checked_megabytes(value, key)).transpose()
}

pub(super) fn parse_optional_bool_field(params: &Value, key: &str) -> Result<Option<bool>> {
    match params.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("'{key}' must be a boolean")),
    }
}

/// How much cleanup a destructive request requires before it reports success.
///
/// `force` selects force mode, which removes the registry record even when host
/// resources could not be released. An absent or `false` value keeps normal
/// mode, where a retained resource fails the request.
pub(super) fn parse_cleanup_mode(params: &Value) -> Result<workspace::CleanupMode> {
    match parse_optional_bool_field(params, "force")? {
        Some(true) => Ok(workspace::CleanupMode::Force),
        _ => Ok(workspace::CleanupMode::Normal),
    }
}

pub(super) fn parse_sandbox_limits_create(params: &Value) -> Result<sandbox::SandboxLimits> {
    let limits = sandbox::SandboxLimits {
        cpu_percent: params.get("cpu_percent").and_then(Value::as_f64),
        memory_bytes: params
            .get("memory_mb")
            .and_then(Value::as_u64)
            .map(|v| v.saturating_mul(1024 * 1024)),
        max_processes: params.get("max_procs").and_then(Value::as_u64),
    };
    limits.validate()?;
    Ok(limits)
}

pub(super) fn parse_sandbox_limits_update(params: &Value) -> Result<sandbox::SandboxLimitsUpdate> {
    Ok(sandbox::SandboxLimitsUpdate {
        cpu_percent: parse_optional_f64_field(params, "cpu_percent")?,
        memory_bytes: parse_optional_u64_field(params, "memory_mb")?
            .map(|value| value.map(|mb| mb.saturating_mul(1024 * 1024))),
        max_processes: parse_optional_u64_field(params, "max_procs")?,
    })
}
