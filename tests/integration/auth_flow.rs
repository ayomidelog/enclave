use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use enclave::auth::AuthManager;
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
        .store_token("github", "ghp_test_token")
        .expect("store token");
    assert!(manager.token_exists("github").expect("token exists"));
    assert_eq!(
        manager.load_token("github").expect("load token"),
        Some("ghp_test_token".to_string())
    );

    assert!(manager.delete_token("github").expect("delete token"));
    assert!(!manager.token_exists("github").expect("token removed"));
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
        .store_token("github", "ghp_workspace_token")
        .expect("store github token");
    manager
        .store_token("enclave", "enc_workspace_token")
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
