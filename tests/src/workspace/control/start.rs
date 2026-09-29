//! What a start owes when it is refused, and what it must leave alone.

use super::*;

/// Build a state directory holding one sandbox with one workspace, and the
/// workspace's directory on disk.
fn fixture(name: &str) -> (std::path::PathBuf, SandboxMetadata, WorkspaceMetadata) {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-start-rollback-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox = sandbox_metadata(&temp_dir);
    let workspace_dir = std::path::PathBuf::from(&sandbox.workspaces_path).join("workspace-id");
    let workspace = workspace_metadata(&sandbox, &workspace_dir, None);
    fs::create_dir_all(&workspace_dir).unwrap();
    fs::create_dir_all(&sandbox.sandbox_path).unwrap();

    let mut registry = crate::registry::Registry::default();
    let mut entry = crate::registry::RegistrySandbox {
        metadata: sandbox.clone(),
        workspaces: BTreeMap::new(),
    };
    entry
        .workspaces
        .insert(workspace.id.clone(), workspace.clone());
    registry.sandboxes.insert(sandbox.id.clone(), entry);
    crate::registry::save_registry_unlocked(&temp_dir, &registry).unwrap();

    (temp_dir, sandbox, workspace)
}

/// A directory left by a refused start is removed when its record is gone.
///
/// This is the shape the wipe/start race leaves: the launch writes the
/// workspace's own record, which creates the directory, and a destroy that has
/// already removed the record leaves that directory named by nothing. The
/// rollback is the last place that can notice, because the destroy has finished.
#[test]
fn a_refused_start_removes_a_directory_whose_record_is_gone() {
    let (state_dir, sandbox, workspace) = fixture("unregistered");
    let workspace_dir = std::path::PathBuf::from(&workspace.workspace_path);
    assert!(
        workspace_dir.is_dir(),
        "the fixture needs a directory to sweep"
    );

    // The destroy wins: the record goes, the directory stays.
    crate::registry::with_registry_mut(&state_dir, |registry| {
        if let Some(entry) = registry.sandboxes.get_mut(&sandbox.id) {
            entry.workspaces.remove(&workspace.id);
        }
        Ok(())
    })
    .unwrap();

    super::super::start::sweep_directory_of_unregistered_workspace(
        &state_dir, &sandbox, &workspace,
    );
    assert!(
        !workspace_dir.exists(),
        "a directory no record describes must not be left behind"
    );

    let _ = fs::remove_dir_all(state_dir);
}

/// A directory is kept when its record is still there.
///
/// The rollback runs on every failed start, including the ones that fail for a
/// reason of their own while the workspace is perfectly healthy. Removing the
/// directory then would delete a live workspace's files, so the sweep has to be
/// conditional on the record being gone and not on the start having failed.
#[test]
fn a_refused_start_keeps_a_directory_whose_record_survived() {
    let (state_dir, sandbox, workspace) = fixture("registered");
    let workspace_dir = std::path::PathBuf::from(&workspace.workspace_path);
    assert!(workspace_dir.is_dir());

    super::super::start::sweep_directory_of_unregistered_workspace(
        &state_dir, &sandbox, &workspace,
    );
    assert!(
        workspace_dir.is_dir(),
        "the directory of a workspace still in the registry must be left alone"
    );

    let _ = fs::remove_dir_all(state_dir);
}
