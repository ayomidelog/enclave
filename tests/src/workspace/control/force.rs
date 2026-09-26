//! What a forced cleanup and a batch destroy report.

use super::*;

#[test]
fn force_cleanup_reports_a_live_runtime_and_keeps_the_workspace_files() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-force-cleanup-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox = sandbox_metadata(&temp_dir);
    let workspace_path = std::path::PathBuf::from(&sandbox.workspaces_path).join("workspace-id");
    fs::create_dir_all(workspace_path.join("fs")).unwrap();
    fs::write(workspace_path.join("fs.img"), "image").unwrap();
    fs::write(workspace_path.join("workspace.json"), "{}").unwrap();
    let workspace = workspace_metadata(&sandbox, &workspace_path, Some(std::process::id()));

    let outcome = cleanup_workspace_artifacts(&sandbox, &workspace, CleanupMode::Force)
        .expect("force cleanup must report the live runtime instead of failing");
    assert!(!outcome.is_complete());
    assert!(!outcome.files_removed);
    assert!(outcome.retained.iter().any(|item| {
        item.resource == "runtime" && item.detail.contains("still alive after stop")
    }));
    // The files are the only remaining description of what is still running, so
    // force mode reports them and leaves them in place.
    assert!(workspace_path.join("fs.img").exists());
    assert!(workspace_path.join("workspace.json").exists());
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn force_destroy_drops_the_record_and_reports_the_live_runtime() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-force-destroy-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    crate::registry::ensure_registry(&temp_dir).unwrap();
    let sandbox = sandbox_metadata(&temp_dir);
    let workspace_path = std::path::PathBuf::from(&sandbox.workspaces_path).join("workspace-id");
    fs::create_dir_all(workspace_path.join("fs")).unwrap();
    fs::write(workspace_path.join("fs.img"), "image").unwrap();
    let workspace = workspace_metadata(&sandbox, &workspace_path, Some(std::process::id()));
    with_registry_mut(&temp_dir, |registry| {
        registry.sandboxes.insert(
            sandbox.id.clone(),
            RegistrySandbox {
                metadata: sandbox.clone(),
                workspaces: BTreeMap::from([(workspace.id.clone(), workspace.clone())]),
            },
        );
        Ok(())
    })
    .unwrap();

    let error =
        destroy_workspace_with_mode(&temp_dir, &sandbox.id, &workspace.id, CleanupMode::Normal)
            .expect_err("normal mode must keep the record while a runtime is still alive");
    assert!(format!("{error:#}").contains("is still alive after stop"));
    with_registry(&temp_dir, |registry| {
        assert!(registry.sandboxes[&sandbox.id]
            .workspaces
            .contains_key(&workspace.id));
        Ok(())
    })
    .unwrap();

    let report =
        destroy_workspace_with_mode(&temp_dir, &sandbox.id, &workspace.id, CleanupMode::Force)
            .expect("force mode must report the retained runtime instead of failing");
    assert_eq!(report.workspace_id, workspace.id);
    assert!(report.mode.is_force());
    assert!(report
        .retained
        .iter()
        .any(|item| item.resource == "runtime"));
    with_registry(&temp_dir, |registry| {
        assert!(!registry.sandboxes[&sandbox.id]
            .workspaces
            .contains_key(&workspace.id));
        Ok(())
    })
    .unwrap();
    assert!(workspace_path.join("fs.img").exists());
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn destroy_all_workspaces_returns_empty_plan_without_spawning_cleanup_workers() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-batch-empty-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);

    crate::registry::ensure_registry(&temp_dir).unwrap();
    let report = destroy_all_workspaces(&temp_dir, CleanupMode::Normal).unwrap();

    assert!(report.removed.is_empty());
    assert!(report.errors.is_empty());
    let _ = fs::remove_dir_all(&temp_dir);
}
