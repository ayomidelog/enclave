use super::*;

pub(super) fn dispatch_workspace_snapshot(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let name = params.get("name").and_then(Value::as_str);
    let result =
        workspace::create_workspace_snapshot(&config.state_dir, sandbox, workspace_selector, name)?;
    Ok(serde_json::to_value(result)?)
}

pub(super) fn dispatch_workspace_restore(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let snapshot = require_param_str(params, &["snapshot"])?;
    let result = workspace::restore_workspace_snapshot(
        &config.state_dir,
        sandbox,
        workspace_selector,
        snapshot,
    )?;
    Ok(serde_json::to_value(result)?)
}

pub(super) fn dispatch_workspace_snapshot_gc(
    params: &Value,
    config: &DaemonConfig,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let keep = params
        .get("keep")
        .and_then(Value::as_u64)
        .unwrap_or(workspace::DEFAULT_SNAPSHOT_KEEP as u64) as usize;
    let removed =
        workspace::gc_workspace_snapshots(&config.state_dir, sandbox, workspace_selector, keep)?;
    Ok(serde_json::to_value(removed)?)
}

pub(super) fn dispatch_workspace_snapshot_export(
    params: &Value,
    config: &DaemonConfig,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let snapshot = require_param_str(params, &["snapshot"])?;
    let output = require_param_str(params, &["output"])?;
    let result = workspace::export_workspace_snapshot_archive(
        &config.state_dir,
        sandbox,
        workspace_selector,
        snapshot,
        std::path::Path::new(output),
    )?;
    Ok(serde_json::to_value(result)?)
}

pub(super) fn dispatch_workspace_snapshot_import(
    params: &Value,
    config: &DaemonConfig,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;
    let archive = require_param_str(params, &["archive"])?;
    let name = params.get("name").and_then(Value::as_str);
    let replace = params
        .get("replace")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let result = workspace::import_workspace_snapshot_archive(
        &config.state_dir,
        sandbox,
        workspace_selector,
        std::path::Path::new(archive),
        name,
        replace,
    )?;
    Ok(serde_json::to_value(result)?)
}
