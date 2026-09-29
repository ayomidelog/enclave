//! Changing a workspace's definition, and what an omitted field means.

use super::*;

/// Build a state directory holding one sandbox with one stopped workspace.
fn fixture(name: &str) -> (std::path::PathBuf, SandboxMetadata, WorkspaceMetadata) {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-definition-update-{name}-{}",
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

/// An update that names an owner binds the workspace to that namespace, one that
/// omits it leaves the binding alone, and one that sends null clears it.
///
/// The three cases are the whole reason `owner` is not a plain `Option`: a
/// definition is applied to a workspace that already exists, so "do not mention
/// it" and "remove it" have to be different instructions. A workspace silently
/// rebound from one namespace to another would be handed a different credential.
#[test]
fn an_owner_binding_is_set_kept_and_cleared() {
    let (state_dir, sandbox, workspace) = fixture("owner");
    let owner_of = |state_dir: &std::path::Path| {
        crate::registry::with_registry(state_dir, |registry| {
            Ok(registry
                .sandboxes
                .get(&sandbox.id)
                .and_then(|entry| entry.workspaces.get(&workspace.id))
                .and_then(|record| record.owner.clone()))
        })
        .expect("read the registry")
    };

    let update = |owner: Option<Option<String>>| {
        update_workspace_definition(
            &state_dir,
            &sandbox.id,
            &workspace.id,
            crate::workspace::WorkspaceDefinitionUpdate {
                owner,
                limits: crate::workspace::WorkspaceLimitsUpdate::default(),
                ..Default::default()
            },
        )
        .expect("update the definition")
    };

    assert_eq!(owner_of(&state_dir), None);

    update(Some(Some("alice".to_string())));
    assert_eq!(owner_of(&state_dir).as_deref(), Some("alice"));

    // Omitted: the binding survives, which is what makes a second `up` from an
    // Enclavefile that does not mention the workspace harmless.
    update(None);
    assert_eq!(owner_of(&state_dir).as_deref(), Some("alice"));

    // Explicitly cleared.
    update(Some(None));
    assert_eq!(owner_of(&state_dir), None);

    let _ = fs::remove_dir_all(state_dir);
}

/// A definition update refuses an owner that could not name a namespace.
///
/// The id becomes a directory name under the auth store, so a workspace bound to
/// something that cannot be one would silently inject nothing.
#[test]
fn an_update_refuses_an_unusable_owner() {
    let (state_dir, sandbox, workspace) = fixture("bad-owner");

    let result = update_workspace_definition(
        &state_dir,
        &sandbox.id,
        &workspace.id,
        crate::workspace::WorkspaceDefinitionUpdate {
            owner: Some(Some("../escape".to_string())),
            limits: crate::workspace::WorkspaceLimitsUpdate::default(),
            ..Default::default()
        },
    );
    assert!(result.is_err(), "a traversing owner must be refused");

    let _ = fs::remove_dir_all(state_dir);
}
