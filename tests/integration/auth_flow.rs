use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use enclave::auth::{AuthManager, TokenScope};
use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    destroy_workspace, exec_workspace_command, start_workspace, stop_workspace,
    WorkspaceCreateOptions, WorkspaceLimits,
};

use super::support::{prepare_cached_rootfs, root_only, state_dir};

#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn auth_login_logout_persists_provider_token() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-auth-login");
    let manager = AuthManager::new(&state);
    manager
        .store_token(&TokenScope::Shared, "github", "ghp_test_token", true)
        .expect("store token");
    assert!(manager
        .token_exists(&TokenScope::Shared, "github")
        .expect("token exists"));
    assert_eq!(
        manager
            .load_token(&TokenScope::Shared, "github")
            .expect("load token"),
        Some("ghp_test_token".to_string())
    );

    assert!(manager
        .delete_token(&TokenScope::Shared, "github")
        .expect("delete token"));
    assert!(!manager
        .token_exists(&TokenScope::Shared, "github")
        .expect("token removed"));
    let _ = fs::remove_dir_all(state);
}

#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn workspace_start_writes_declared_auth_token_file() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-auth-workspace");
    prepare_cached_rootfs(&state, "bookworm");
    let manager = AuthManager::new(&state);
    manager
        .store_token(&TokenScope::Shared, "github", "ghp_workspace_token", true)
        .expect("store github token");
    manager
        .store_token(&TokenScope::Shared, "enclave", "enc_workspace_token", true)
        .expect("store enclave token");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-auth-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let workspace = enclave::workspace::create_workspace_with_options(
        &state,
        &sandbox.id,
        "dev",
        enclave::workspace::WorkspaceCreateOptions {
            limits: WorkspaceLimits::default(),
            auth_providers: vec!["github".to_string(), "npm".to_string()],
            env_tokens: vec!["ENCLAVE_TOKEN".to_string()],
            published_ports: vec![],
            ..enclave::workspace::WorkspaceCreateOptions::default()
        },
    )
    .expect("create workspace");

    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let runtime_pid = started.runtime_pid.expect("runtime pid should be set");
    let workspace_root = Path::new("/proc")
        .join(runtime_pid.to_string())
        .join("root");

    let auth_base = workspace_root.join("run/enclave/auth");
    let token_file = auth_base.join("github.token");
    assert!(token_file.exists(), "github token file should be present");
    let mode = fs::metadata(&token_file)
        .expect("token metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o400);
    let token_content = fs::read_to_string(&token_file).expect("read token content");
    assert_eq!(token_content, "ghp_workspace_token");

    let missing_file = auth_base.join("npm.token");
    assert!(
        !missing_file.exists(),
        "missing provider token should not block startup and should not be injected"
    );
    let env_base = workspace_root.join("run/enclave/env");
    let env_token_file = env_base.join("ENCLAVE_TOKEN");
    assert!(
        env_token_file.exists(),
        "environment token file should be present"
    );
    let env_token_content = fs::read_to_string(&env_token_file).expect("read env token content");
    assert_eq!(env_token_content, "enc_workspace_token");
    let shared_rootfs_token =
        Path::new(&workspace.sandbox_rootfs_path).join("run/enclave/auth/github.token");
    assert!(
        !shared_rootfs_token.exists(),
        "auth token should not be written into shared sandbox rootfs"
    );
    let shared_rootfs_env_token =
        Path::new(&workspace.sandbox_rootfs_path).join("run/enclave/env/ENCLAVE_TOKEN");
    assert!(
        !shared_rootfs_env_token.exists(),
        "environment token should not be written into shared sandbox rootfs"
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

/// A workspace can ask for a credential the provider table does not contain, and
/// the value it prints is scrubbed out of the captured output.
///
/// This is the end-to-end shape of the feature: a slot stored under a free-form
/// name, a workspace that names the variable it wants, and output that comes back
/// with the value removed. The value is a canary, so finding it anywhere in the
/// output or in the audit log is the failure.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_free_form_environment_token_is_injected_and_scrubbed() {
    if !root_only() {
        return;
    }

    const CANARY: &str = "hunter2-canary-value";
    let state = state_dir("enclave-int-auth-free-form");
    prepare_cached_rootfs(&state, "bookworm");
    let manager = AuthManager::new(&state);
    manager
        .store_token(
            &TokenScope::User("alice".to_string()),
            "netflix-password",
            CANARY,
            false,
        )
        .expect("store the slot");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-auth-free-form-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let workspace = enclave::workspace::create_workspace_with_options(
        &state,
        &sandbox.id,
        "vault",
        WorkspaceCreateOptions {
            limits: WorkspaceLimits::default(),
            owner: Some("alice".to_string()),
            env_tokens: vec!["NETFLIX_PASSWORD".to_string()],
            ..WorkspaceCreateOptions::default()
        },
    )
    .expect("create workspace");

    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let pid = started.runtime_pid.expect("runtime pid");
    let env_token_path = Path::new("/proc")
        .join(pid.to_string())
        .join("root/run/enclave/env/NETFLIX_PASSWORD");
    assert_eq!(
        fs::read_to_string(&env_token_path).expect("read the injected variable"),
        CANARY,
        "the slot's value must reach the workspace under the variable it asked for"
    );

    let result = exec_workspace_command(
        &state,
        &sandbox.id,
        &workspace.id,
        "/home",
        &[
            "sh".to_string(),
            "-c".to_string(),
            "printf 'value=%s' \"$NETFLIX_PASSWORD\"".to_string(),
        ],
    )
    .expect("execute a command that prints the variable");
    assert_eq!(
        result.exit_code, 0,
        "stdout={} stderr={}",
        result.stdout, result.stderr
    );
    assert!(
        result.stdout.contains("[REDACTED]"),
        "the value must be replaced in the output: {}",
        result.stdout
    );
    assert!(
        !result.stdout.contains(CANARY),
        "the value must not survive into the output: {}",
        result.stdout
    );

    let log = fs::read_to_string(state.join("auth/audit.log")).expect("read the audit log");
    assert!(
        !log.contains(CANARY),
        "the audit log must never contain a value: {log}"
    );
    let injects: Vec<serde_json::Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).expect("every audit line is json"))
        .filter(|event: &serde_json::Value| event["action"] == "inject")
        .collect();
    // One for the start, one for the command: the same credential reaching the
    // workspace twice is two events, and a command that resolved nothing would
    // have added none.
    assert_eq!(injects.len(), 2, "one inject per operation: {log}");
    for event in injects {
        assert_eq!(event["provider"], "netflix-password");
        assert_eq!(event["user"], "alice");
        assert_eq!(event["workspace"], workspace.id);
    }

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

/// Revoking a credential takes effect on the next command, not the next restart.
///
/// The wrapper inside the workspace re-reads its env directory for every command,
/// so re-resolving the store before each command is what makes a revocation
/// immediate. Without it the workspace would keep handing out a credential the
/// store no longer has until it was restarted.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_revoked_environment_token_is_gone_from_the_next_command() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-auth-revoked");
    prepare_cached_rootfs(&state, "bookworm");
    let manager = AuthManager::new(&state);
    let alice = TokenScope::User("alice".to_string());
    manager
        .store_token(&alice, "netflix-password", "hunter2", false)
        .expect("store the slot");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-auth-revoked-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let workspace = enclave::workspace::create_workspace_with_options(
        &state,
        &sandbox.id,
        "vault",
        WorkspaceCreateOptions {
            limits: WorkspaceLimits::default(),
            owner: Some("alice".to_string()),
            env_tokens: vec!["NETFLIX_PASSWORD".to_string()],
            ..WorkspaceCreateOptions::default()
        },
    )
    .expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let pid = started.runtime_pid.expect("runtime pid");
    let env_token_path = Path::new("/proc")
        .join(pid.to_string())
        .join("root/run/enclave/env/NETFLIX_PASSWORD");
    assert!(env_token_path.exists(), "the token starts out injected");

    assert!(
        manager
            .delete_token(&alice, "netflix-password")
            .expect("revoke"),
        "the slot must have been there to remove"
    );

    let result = exec_workspace_command(
        &state,
        &sandbox.id,
        &workspace.id,
        "/home",
        &[
            "sh".to_string(),
            "-c".to_string(),
            "printf 'value=[%s]' \"$NETFLIX_PASSWORD\"".to_string(),
        ],
    )
    .expect("execute a command after the revocation");
    assert_eq!(
        result.exit_code, 0,
        "stdout={} stderr={}",
        result.stdout, result.stderr
    );
    assert_eq!(
        result.stdout, "value=[]",
        "a revoked credential must not be exported: {}",
        result.stdout
    );
    assert!(
        !env_token_path.exists(),
        "the revoked token must be removed from the workspace, not left behind"
    );

    // The start recorded its injection; the command after the revocation injected
    // nothing, so it recorded nothing.
    let log = fs::read_to_string(state.join("auth/audit.log")).expect("read the audit log");
    let injects = log
        .lines()
        .filter(|line| line.contains("\"action\":\"inject\""))
        .count();
    assert_eq!(injects, 1, "one inject, from the start: {log}");

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

/// A credential the workspace no longer declares must be removed, not left behind.
///
/// Skipping an unchanged write is only safe if the reconcile still removes what is
/// no longer wanted. A token file left in place after the provider was dropped is a
/// credential the workspace should not still hold, so this pins the other half of
/// the same change.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_dropped_provider_token_is_removed_on_restart() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-auth-dropped");
    prepare_cached_rootfs(&state, "bookworm");
    let manager = AuthManager::new(&state);
    manager
        .store_token(&TokenScope::Shared, "github", "ghp_dropped_token", true)
        .expect("store github token");
    manager
        .store_token(&TokenScope::Shared, "enclave", "enc_kept_token", true)
        .expect("store enclave token");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-auth-dropped-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = enclave::workspace::create_workspace_with_options(
        &state,
        &sandbox.id,
        "dev",
        enclave::workspace::WorkspaceCreateOptions {
            limits: WorkspaceLimits::default(),
            auth_providers: vec!["github".to_string(), "enclave".to_string()],
            ..enclave::workspace::WorkspaceCreateOptions::default()
        },
    )
    .expect("create workspace");

    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let pid = started.runtime_pid.expect("runtime pid");
    let auth_dir = Path::new("/proc")
        .join(pid.to_string())
        .join("root/run/enclave/auth");
    assert!(auth_dir.join("github.token").exists());
    assert!(auth_dir.join("enclave.token").exists());

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");

    // Drop github, keeping enclave, and start again.
    enclave::registry::with_registry_mut(&state, |registry| {
        let workspace = registry
            .sandboxes
            .get_mut(&sandbox.id)
            .and_then(|sandbox| sandbox.workspaces.get_mut(&workspace.id))
            .expect("workspace record");
        workspace.auth_providers = vec!["enclave".to_string()];
        Ok(())
    })
    .expect("drop the github provider");

    let restarted =
        start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace again");
    let pid = restarted.runtime_pid.expect("runtime pid");
    let auth_dir = Path::new("/proc")
        .join(pid.to_string())
        .join("root/run/enclave/auth");
    assert!(
        !auth_dir.join("github.token").exists(),
        "the token for a provider the workspace no longer declares is still there"
    );
    assert_eq!(
        fs::read_to_string(auth_dir.join("enclave.token")).expect("read the kept token"),
        "enc_kept_token"
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

/// Each workspace is given its owner's token, and only its owner's.
///
/// This is the whole point of the feature: two workspaces on one sandbox that
/// declare the same provider must not end up with the same credential. The
/// assertion is on the file the workspace actually holds, because that is what
/// the wrapper reads when a command runs.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_workspace_is_given_its_owners_token_and_not_another_users() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-auth-owner");
    prepare_cached_rootfs(&state, "bookworm");
    let manager = AuthManager::new(&state);
    manager
        .store_token(
            &TokenScope::User("alice".to_string()),
            "github",
            "alice-token",
            false,
        )
        .expect("store alice's token");
    manager
        .store_token(
            &TokenScope::User("bob".to_string()),
            "github",
            "bob-token",
            false,
        )
        .expect("store bob's token");
    manager
        .store_token(&TokenScope::Shared, "github", "shared-token", false)
        .expect("store the shared token");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-auth-owner-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let create = |name: &str, owner: Option<&str>| {
        let workspace = enclave::workspace::create_workspace_with_options(
            &state,
            &sandbox.id,
            name,
            enclave::workspace::WorkspaceCreateOptions {
                limits: WorkspaceLimits::default(),
                auth_providers: vec!["github".to_string()],
                owner: owner.map(str::to_string),
                ..enclave::workspace::WorkspaceCreateOptions::default()
            },
        )
        .expect("create workspace");
        let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
        let pid = started.runtime_pid.expect("runtime pid");
        let token_path = Path::new("/proc")
            .join(pid.to_string())
            .join("root/run/enclave/auth/github.token");
        let contents = fs::read_to_string(&token_path).ok();
        (workspace.id, contents)
    };

    let (alice_workspace, alice_token) = create("alice-ws", Some("alice"));
    let (bob_workspace, bob_token) = create("bob-ws", Some("bob"));
    let (plain_workspace, plain_token) = create("plain-ws", None);

    assert_eq!(alice_token.as_deref(), Some("alice-token"));
    assert_eq!(bob_token.as_deref(), Some("bob-token"));
    // No owner keeps the namespace every workspace used before this existed.
    assert_eq!(plain_token.as_deref(), Some("shared-token"));

    for workspace in [&alice_workspace, &bob_workspace, &plain_workspace] {
        stop_workspace(&state, &sandbox.id, workspace).expect("stop workspace");
        destroy_workspace(&state, &sandbox.id, workspace).expect("destroy workspace");
    }
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
