//! Starting many workspaces at once, and the retry that makes it idempotent.
//!
//! A batch start runs the items on a bounded pool of threads, because the work
//! is dominated by host commands and mounts. Each item reports its own outcome,
//! so one failure does not hide the rest.

use super::*;

pub(in crate::daemon::dispatch) fn dispatch_workspace_start_many(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let specs = params
        .get("workspaces")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("missing 'workspaces' array"))?
        .clone();
    if specs.is_empty() {
        return Ok(json!([]));
    }

    let worker_count = std::env::var("ENCLAVE_UP_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=64).contains(value))
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|parallelism| parallelism.get().max(1))
                .unwrap_or(4)
        })
        .min(specs.len());
    let queue = std::sync::Arc::new(std::sync::Mutex::new(
        std::collections::VecDeque::from_iter(specs.into_iter().enumerate()),
    ));
    let result_count = queue.lock().map(|queue| queue.len()).unwrap_or(0);
    let results = std::sync::Arc::new(std::sync::Mutex::new(
        (0..result_count)
            .map(|_| None::<Result<Value>>)
            .collect::<Vec<_>>(),
    ));

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = std::sync::Arc::clone(&queue);
            let results = std::sync::Arc::clone(&results);
            let config = config.clone();
            let port_publisher = Arc::clone(port_publisher);
            scope.spawn(move || loop {
                let Some((index, spec)) = queue.lock().ok().and_then(|mut queue| queue.pop_front())
                else {
                    return;
                };
                let result = dispatch_workspace_start_many_item(&spec, &config, &port_publisher);
                if let Ok(mut results) = results.lock() {
                    results[index] = Some(result);
                }
            });
        }
    });

    let results = Arc::try_unwrap(results)
        .map_err(|_| anyhow::anyhow!("workspace batch result ownership remained active"))?
        .into_inner()
        .map_err(|_| anyhow::anyhow!("workspace batch result lock poisoned"))?;
    let mut output = Vec::with_capacity(results.len());
    for (index, result) in results.into_iter().enumerate() {
        let result =
            result.ok_or_else(|| anyhow::anyhow!("workspace batch item did not complete"))?;
        output.push(match result {
            Ok(value) => json!({ "index": index, "ok": true, "result": value }),
            Err(error) => json!({
                "index": index,
                "ok": false,
                "error": format!("{error:#}"),
            }),
        });
    }
    Ok(Value::Array(output))
}

pub(in crate::daemon::dispatch) fn dispatch_workspace_start_many_item(
    spec: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let started = match dispatch_workspace_create(spec, config, port_publisher) {
        Ok(result) => result,
        Err(error) => {
            let sandbox = require_param_str(spec, &["sandbox_id"])?;
            let workspace = require_param_str(spec, &["name"])?;
            let exists = workspace::list_workspaces(&config.state_dir, Some(sandbox))?
                .iter()
                .any(|item| item.name == workspace || item.id == workspace);
            if !exists {
                return Err(error);
            }
            let update = existing_workspace_update(spec, sandbox, workspace);
            dispatch_workspace_update(&update, config, port_publisher)?;
            dispatch_workspace_target(
                &json!({ "sandbox": sandbox, "workspace": workspace }),
                config,
                "start",
                port_publisher,
            )?
        }
    };

    if let Some(run_command) = spec.get("run").and_then(Value::as_str) {
        let metadata: workspace::WorkspaceMetadata = serde_json::from_value(started.clone())?;
        workspace::spawn_workspace_command_detached(
            &metadata,
            "/home",
            &["sh".to_string(), "-c".to_string(), run_command.to_string()],
        )
        .with_context(|| {
            format!(
                "failed to launch run command for workspace '{}'",
                metadata.name
            )
        })?;
    }
    Ok(started)
}

/// Translate a `workspace.start_many` item into a `workspace.update` request
/// for a workspace that already exists.
///
/// The two requests share parameter names but not semantics. A start item
/// carries the declared definition, including `disk_mb`, while an update
/// cannot change an existing disk allocation at all. Forwarding the declared
/// size would make every `up` after a `down` fail for quota-backed workspaces.
pub(in crate::daemon::dispatch) fn existing_workspace_update(
    spec: &Value,
    sandbox: &str,
    workspace: &str,
) -> Value {
    let mut update = spec.clone();
    let Some(object) = update.as_object_mut() else {
        return update;
    };
    for key in [
        "cpu_seconds",
        "cpu_percent",
        "memory_mb",
        "max_procs",
        "max_open_files",
        "path",
    ] {
        if object.get(key).is_some_and(Value::is_null) {
            object.remove(key);
        }
    }
    object.remove("disk_mb");
    object.insert("sandbox".to_string(), Value::String(sandbox.to_string()));
    object.insert(
        "workspace".to_string(),
        Value::String(workspace.to_string()),
    );
    update
}
