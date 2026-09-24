use super::*;

/// What a workspace destroy removed and what it had to leave behind.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceDestroyReport {
    pub workspace_id: String,
    pub mode: cleanup::CleanupMode,
    /// Resources that are still held, reported in force mode.
    #[serde(default)]
    pub retained: Vec<cleanup::RetainedResource>,
    /// What the host looked like after the destroy finished.
    ///
    /// A removed directory and a removed registry record are not evidence that
    /// the workspace's veth, rules, mounts, or loop device are gone. The
    /// certificate is that evidence, and it is checked against the record as it
    /// was immediately before deletion.
    #[serde(default)]
    pub certificate: crate::workspace::WorkspaceCleanupCertificate,
}

impl WorkspaceDestroyReport {
    /// One line naming everything that was left behind, for command output.
    pub fn retained_summary(&self) -> String {
        self.retained
            .iter()
            .map(|item| format!("{}: {}", item.resource, item.detail))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

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
    destroy_workspace_with_mode(
        state_dir,
        sandbox_selector,
        workspace_selector,
        CleanupMode::Normal,
    )
    .map(|report| report.workspace_id)
}

/// Destroy a workspace and report what was released and what was retained.
pub fn destroy_workspace_with_mode(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    mode: CleanupMode,
) -> Result<WorkspaceDestroyReport> {
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
    let outcome = match cleanup::cleanup_workspace_artifacts(&sandbox, &workspace, mode) {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = journal.fail(format!("{error:#}"));
            return Err(error);
        }
    };
    if !outcome.retained.is_empty() {
        // Force mode removes the registry record and leaves the host state to
        // `doctor --repair`, so the journal has to name what was left behind.
        journal.phase("retain_unreleased_resources")?;
        tracing::warn!(
            "workspace '{}' destroy left resources behind: {}",
            workspace_id,
            outcome.retained_summary()
        );
    }

    // Verify the host after deletion rather than inferring it from the removal
    // calls: a directory removal succeeding is not evidence that the loop device
    // was detached or that the mounts are gone. In normal mode the artifact
    // cleanup already refused to delete anything it could not release, so this
    // catches what only becomes visible once the files are gone. It runs before
    // the registry record is removed, so a failure keeps the evidence.
    let network_complete = !outcome
        .retained
        .iter()
        .any(|retained| retained.resource == "network");
    let certificate = crate::workspace::verify_workspace_destroyed(&workspace, network_complete);
    if !mode.is_force() && !certificate.is_complete() {
        let _ = journal.fail(format!(
            "destroy verification failed: {}",
            certificate.failure_summary()
        ));
        bail!(
            "workspace '{}' destroy left resources behind, retaining its registry record: {}",
            workspace_id,
            certificate.failure_summary()
        );
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

    Ok(WorkspaceDestroyReport {
        workspace_id,
        mode,
        retained: outcome.retained,
        certificate,
    })
}

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
                let result = cleanup::cleanup_workspace_artifacts(sandbox, workspace, mode)
                    .and_then(|outcome| {
                        let workspace_id = workspace.id.clone();
                        with_registry_mut(state_dir, |registry| {
                            if let Some(sandbox) = registry.sandboxes.get_mut(&sandbox.id) {
                                sandbox.workspaces.remove(&workspace_id);
                            }
                            Ok((workspace_id, outcome))
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
            Ok((workspace_id, outcome)) => {
                if !outcome.is_complete() {
                    report
                        .retained
                        .insert(workspace_id.clone(), outcome.retained);
                }
                report.removed.push(workspace_id);
            }
            Err(error) => report.errors.push(format!("{error:#}")),
        }
    }
    Ok(report)
}
