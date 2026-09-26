//! Running a setup command inside a sandbox, through the setup cache.
//!
//! The cache decision is the caller in the command layer; this validates the
//! request and hands the command and its cache inputs to the sandbox layer.

use super::*;

/// Run a setup command inside a sandbox, optionally through the setup cache.
pub(super) fn dispatch_sandbox_exec_setup(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let selector = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let command = require_param_str(params, &["command"])?;
    let cache_setup = params
        .get("cache_setup")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let setup_digest = params.get("setup_digest").and_then(Value::as_str);
    let setup_commands = params
        .get("setup_commands")
        .and_then(Value::as_array)
        .map(|commands| {
            commands
                .iter()
                .map(|command| {
                    command
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| anyhow::anyhow!("setup_commands entries must be strings"))
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    let setup_index = params.get("setup_index").and_then(Value::as_u64);
    sandbox::exec_setup_command(
        &config.state_dir,
        selector,
        command,
        cache_setup,
        setup_commands.as_deref(),
        setup_digest,
        setup_index,
    )
}
