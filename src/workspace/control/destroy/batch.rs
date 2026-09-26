//! Releasing every workspace in every sandbox.

use super::super::*;

use super::one::destroy_one;
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct BatchDestroyReport {
    pub removed: Vec<String>,
    pub errors: Vec<String>,
    /// Resources a force wipe could not release, keyed by workspace id.
    #[serde(default)]
    pub retained: std::collections::BTreeMap<String, Vec<cleanup::RetainedResource>>,
}

pub fn destroy_all_workspaces(
    state_dir: &std::path::Path,
    mode: CleanupMode,
) -> Result<BatchDestroyReport> {
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
                let workspace_id = workspace.id.clone();
                // The plan is a snapshot, so each workspace is destroyed from its
                // current record: a workspace that started since the plan was taken
                // has a runtime this pass has to stop rather than a record to delete.
                let result = destroy_one(state_dir, &sandbox.id, &workspace_id, mode, None).map(
                    |destroyed| {
                        // The id comes from the record that was destroyed rather than
                        // from the plan, so a workspace the plan named but a competing
                        // operation already removed is reported as gone rather than as
                        // this wipe's work.
                        destroyed.map(|destroyed| (destroyed.workspace.id, destroyed.outcome))
                    },
                );
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
            Ok(Some((workspace_id, outcome))) => {
                if !outcome.is_complete() {
                    report
                        .retained
                        .insert(workspace_id.clone(), outcome.retained);
                }
                report.removed.push(workspace_id);
            }
            // The workspace was already gone, which is the outcome a wipe wants.
            Ok(None) => {}
            Err(error) => report.errors.push(format!("{error:#}")),
        }
    }
    Ok(report)
}
