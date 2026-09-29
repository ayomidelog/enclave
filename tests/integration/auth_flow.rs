use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use enclave::auth::{AuthManager, TokenScope};
use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{destroy_workspace, start_workspace, stop_workspace, WorkspaceLimits};

fn root_only() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn prepare_cached_rootfs(state_dir: &Path, suite: &str) {
    let cache = state_dir.join("sandboxes").join("rootfs-cache").join(suite);
    fs::create_dir_all(cache.join("bin")).expect("create bin");
    fs::create_dir_all(cache.join("etc")).expect("create etc");
    fs::create_dir_all(cache.join("usr")).expect("create usr");
    fs::write(cache.join("bin/sh"), "#!/bin/sh\nexit 0\n").expect("write shell");
}

fn state_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create state dir");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("secure state dir");
    dir
}

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
