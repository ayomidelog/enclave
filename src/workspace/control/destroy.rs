use super::*;

pub fn remove_workspace(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<()> {
    destroy_workspace(state_dir, sandbox_selector, workspace_selector).map(|_| ())
}

pub fn destroy_workspace(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<String> {
    let (sandbox, workspace) = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;

        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "workspace '{}' not found in sandbox '{}'",
                    workspace_id,
                    sandbox_id
                )
            })?;

        Ok((sandbox.metadata.clone(), workspace))
    })?;

    let workspace_id = workspace.id.clone();
    let mut journal = crate::operation::Journal::begin(
        state_dir,
        "workspace.destroy",
        format!("{}/{}", sandbox.id, workspace_id),
    )?;
    journal.phase("cleanup")?;
    if let Err(error) = cleanup::cleanup_workspace_artifacts(&sandbox, &workspace) {
        let _ = journal.fail(format!("{error:#}"));
        return Err(error);
    }
    journal.phase("remove_registry_record")?;

    if let Err(error) = with_registry_mut(state_dir, |registry| {
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox.id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox.id))?;
        sandbox.workspaces.remove(&workspace_id);
        Ok(())
    }) {
        let _ = journal.fail(format!("{error:#}"));
        return Err(error);
    }
    journal.succeed()?;

    Ok(workspace_id)
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct BatchDestroyReport {
    pub removed: Vec<String>,
    pub errors: Vec<String>,
}

pub fn destroy_all_workspaces(state_dir: &std::path::Path) -> Result<BatchDestroyReport> {
    let plan = with_registry(state_dir, |registry| {
        let mut plan = Vec::new();
        for sandbox in registry.sandboxes.values() {
            for workspace in sandbox.workspaces.values() {
                plan.push((sandbox.metadata.clone(), workspace.clone()));
            }
        }
        Ok(plan)
    })?;

    if plan.is_empty() {
        return Ok(BatchDestroyReport::default());
    }

    let plan = Arc::new(plan);
    let worker_count = std::env::var("ENCLAVE_CLEANUP_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(4)
        .clamp(1, 64)
        .min(plan.len());
    let queue = Arc::new(Mutex::new(VecDeque::from_iter(0..plan.len())));
    let results = Arc::new(Mutex::new(
        (0..plan.len()).map(|_| None).collect::<Vec<_>>(),
    ));

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = Arc::clone(&queue);
            let plan = Arc::clone(&plan);
            let results = Arc::clone(&results);
            scope.spawn(move || loop {
                let Some(index) = queue.lock().ok().and_then(|mut queue| queue.pop_front()) else {
                    break;
                };
                let (sandbox, workspace) = &plan[index];
                let result =
                    cleanup::cleanup_workspace_artifacts(sandbox, workspace).and_then(|()| {
                        let workspace_id = workspace.id.clone();
                        with_registry_mut(state_dir, |registry| {
                            if let Some(sandbox) = registry.sandboxes.get_mut(&sandbox.id) {
                                sandbox.workspaces.remove(&workspace_id);
                            }
                            Ok(workspace_id)
                        })
                    });
                if let Ok(mut results) = results.lock() {
                    results[index] = Some(result);
                }
            });
        }
    });

    let results = Arc::try_unwrap(results)
        .map_err(|_| anyhow!("workspace cleanup result ownership leaked"))?
        .into_inner()
        .map_err(|_| anyhow!("workspace cleanup result lock poisoned"))?;
    let mut report = BatchDestroyReport::default();
    for result in results.into_iter().flatten() {
        match result {
            Ok(workspace_id) => report.removed.push(workspace_id),
            Err(error) => report.errors.push(format!("{error:#}")),
        }
    }
    Ok(report)
}
