use std::fs;
use std::os::unix::fs::PermissionsExt;

use enclave::auth::{
    provider_env_var, provider_for_env_var, workspace_env_wrapper_script, AuthManager,
};

fn temp_state_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "enclave-auth-unit-{}-{}-{}",
        name,
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).expect("create temp state dir");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("set state dir perms");
    dir
}

#[test]
fn token_storage_rejects_invalid_provider_path_values() {
    let state_dir = temp_state_dir("invalid-provider");
    let manager = AuthManager::new(&state_dir);
    let result = manager.store_token("../escape", "abc");
    assert!(result.is_err());
    let _ = fs::remove_dir_all(state_dir);
}

#[test]
fn token_load_rejects_insecure_permissions() {
    let state_dir = temp_state_dir("bad-perms");
    let auth_dir = state_dir.join("auth");
    fs::create_dir_all(&auth_dir).expect("create auth dir");
    fs::set_permissions(&auth_dir, fs::Permissions::from_mode(0o700)).expect("set auth perms");
    let token_path = auth_dir.join("github.token");
    fs::write(&token_path, "secret").expect("write token");
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o644)).expect("set bad perms");

    let manager = AuthManager::new(&state_dir);
    let result = manager.load_token("github");
    assert!(result.is_err());

    let _ = fs::remove_dir_all(state_dir);
}

#[test]
fn token_load_accepts_current_user_owned_file_with_strict_mode() {
    let state_dir = temp_state_dir("good-owner");
    let manager = AuthManager::new(&state_dir);
    manager
        .store_token("github", "secret")
        .expect("store token for current user");

    let loaded = manager.load_token("github").expect("load token");
    assert_eq!(loaded.as_deref(), Some("secret"));

    let configured = manager.list_providers().expect("list configured providers");
    assert_eq!(configured, vec!["github".to_string()]);

    let _ = fs::remove_dir_all(state_dir);
}

#[test]
fn list_providers_ignores_insecure_token_files() {
    let state_dir = temp_state_dir("list-insecure");
    let auth_dir = state_dir.join("auth");
    fs::create_dir_all(&auth_dir).expect("create auth dir");
    fs::set_permissions(&auth_dir, fs::Permissions::from_mode(0o700)).expect("set auth perms");
    let token_path = auth_dir.join("github.token");
    fs::write(&token_path, "secret").expect("write token");
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o644)).expect("set bad perms");

    let manager = AuthManager::new(&state_dir);
    let providers = manager.list_providers().expect("list providers");
    assert!(providers.is_empty(), "insecure tokens must not be reported");

    let _ = fs::remove_dir_all(state_dir);
}

#[test]
fn provider_lookup_maps_environment_variable_names() {
    assert_eq!(provider_env_var("enclave"), Some("ENCLAVE_TOKEN"));
    assert_eq!(provider_env_var("github"), Some("GITHUB_TOKEN"));
    assert_eq!(provider_env_var("npm"), Some("NPM_TOKEN"));
    assert_eq!(provider_for_env_var(" enclave_token "), Some("enclave"));
    assert_eq!(provider_for_env_var("github_token"), Some("github"));
    assert_eq!(provider_env_var("gitlab"), None);
}

#[test]
fn workspace_wrapper_configures_git_and_gh_auth_environment() {
    let wrapper = workspace_env_wrapper_script();
    assert!(wrapper.contains("GITHUB_TOKEN"));
    assert!(wrapper.contains("GH_TOKEN"));
    assert!(wrapper.contains("NPM_TOKEN"));
    assert!(wrapper.contains("ENCLAVE_TOKEN"));
    assert!(
        wrapper.contains("credential.helper"),
        "wrapper must configure git credential helper for HTTPS auth"
    );
    assert!(wrapper.contains("GIT_TERMINAL_PROMPT=0"));
    assert!(wrapper.contains("GIT_CONFIG_COUNT"));
    assert!(wrapper.contains("/run/enclave/env/*"));
    assert!(wrapper.contains("export \"${env_name}=${token}\""));
}

#[test]
fn sync_workspace_auth_rejects_non_proc_namespace_root_path() {
    let state_dir = temp_state_dir("sync-auth-path");
    let manager = AuthManager::new(&state_dir);
    let result = manager.sync_workspace_auth("/tmp/not-a-workspace-root", &[], &[]);
    assert!(result.is_err());
    let _ = fs::remove_dir_all(state_dir);
}
