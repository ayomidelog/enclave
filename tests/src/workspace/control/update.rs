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

/// A definition update refuses any request that mentions the disk, in either form.
///
/// `disk_bytes` is `Option<Option<u64>>`, and the guard tests `is_some()`, so both
/// `Some(None)` ("clear it") and `Some(Some(x))` ("set it") are refused. That is what
/// made the rollback that restores a previous definition dead code: it passed
/// `Some(previous)`, which is always `is_some()`, so the restore always failed and the
/// caller only logged it. The rollback now passes `None`, which is what this pins.
#[test]
fn an_update_mentioning_the_disk_is_refused_in_either_form() {
    let (state_dir, sandbox, workspace) = fixture("disk-mention");

    let update = |disk_bytes: Option<Option<u64>>| {
        update_workspace_definition(
            &state_dir,
            &sandbox.id,
            &workspace.id,
            crate::workspace::WorkspaceDefinitionUpdate {
                limits: crate::workspace::WorkspaceLimitsUpdate {
                    disk_bytes,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
    };

    for mentioned in [Some(None), Some(Some(64 * 1024 * 1024))] {
        let err = update(mentioned).expect_err("an update that mentions the disk must be refused");
        assert!(
            err.to_string().contains("workspace resize"),
            "unexpected error: {err:#}"
        );
    }

    // `None` is "leave the disk alone", which is the only form a definition update
    // may use, and is what the rollback now sends.
    update(None).expect("an update that leaves the disk alone is accepted");

    let _ = fs::remove_dir_all(state_dir);
}

/// A memory-only resize is not refused for the disk a host-backed workspace has not got.
///
/// The guard that refuses a disk resize on a workspace whose `/home` is a host
/// directory belongs to the disk half of a resize. Reading the plan ran it
/// unconditionally, so `--memory-mb` and `--no-memory-limit` failed on every workspace
/// created with `workspace_dir`/`path` — which is the common case for a project
/// directory, and the case the guard was never meant to touch.
#[test]
fn a_memory_only_resize_is_allowed_on_a_host_backed_workspace() {
    let (state_dir, sandbox, workspace) = fixture("memory-host-backed");
    // A host-backed workspace with a memory limit, which is the shape the resize
    // used to reject before it had even looked at what was asked for.
    crate::registry::with_registry_mut(&state_dir, |registry| {
        let entry = registry
            .sandboxes
            .get_mut(&sandbox.id)
            .expect("sandbox record");
        // The resize requires a running sandbox, which is the state an operator
        // would be in when they resize a workspace in it.
        entry.metadata.status = SandboxStatus::Running;
        let record = entry
            .workspaces
            .get_mut(&workspace.id)
            .expect("workspace record");
        record.home_mount_source_path = Some("/host/project".to_string());
        record.limits.memory_bytes = Some(512 * 1024 * 1024);
        Ok(())
    })
    .expect("mark the workspace host-backed");

    let result = crate::workspace::control::update::resize_workspace_memory(
        &state_dir,
        &sandbox.id,
        &workspace.id,
        Some(1024 * 1024 * 1024),
    );

    assert!(
        result.is_ok(),
        "a memory-only resize must not be refused for the disk it has not got: {result:?}"
    );

    // And the disk half still refuses it, so the guard was moved rather than removed.
    let disk = crate::workspace::control::update::resize_workspace_disk(
        &state_dir,
        &sandbox.id,
        &workspace.id,
        crate::workspace::storage::MIN_DISK_BYTES * 2,
    );
    let err = disk.expect_err("a disk resize of a host-backed workspace must be refused");
    assert!(
        err.to_string().contains("host-backed workspace directory"),
        "unexpected error: {err:#}"
    );

    let _ = fs::remove_dir_all(state_dir);
}
