//! Per-user auth namespaces: which namespace a token lands in, and which one a
//! workspace resolves against.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use enclave::auth::{validate_user_id, AuthManager, TokenScope};

fn temp_state_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "enclave-auth-ns-{}-{}-{}",
        name,
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).expect("create temp state dir");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("set state dir perms");
    dir
}

#[test]
fn a_user_id_is_validated_against_the_url_safe_set() {
    for valid in ["alice", "+234", "a-b_c.d", "A1", "9"] {
        assert!(validate_user_id(valid).is_ok(), "{valid} must be accepted");
    }
    for invalid in ["", ".", "..", "a/b", "a b", "a\\b", "a:b", "a\0b"] {
        assert!(
            validate_user_id(invalid).is_err(),
            "{invalid:?} must be rejected"
        );
    }
    // The id becomes one directory name, so an over-long one is refused rather
    // than left to fail when the path is built.
    assert!(validate_user_id(&"a".repeat(65)).is_err());
    assert!(validate_user_id(&"a".repeat(64)).is_ok());
}

#[test]
fn a_user_token_lives_in_its_own_namespace() {
    let state_dir = temp_state_dir("isolation");
    let manager = AuthManager::new(&state_dir);
    let alice = TokenScope::User("alice".to_string());
    let bob = TokenScope::User("bob".to_string());

    manager
        .store_token(&alice, "github", "alice-token", false)
        .expect("store alice's token");

    assert_eq!(
        manager
            .load_token(&alice, "github")
            .expect("load alice's token")
            .as_deref(),
        Some("alice-token")
    );
    // The whole point: another user's namespace, and the shared one, must not
    // resolve to alice's token.
    assert_eq!(
        manager
            .load_token(&bob, "github")
            .expect("load bob's token"),
        None
    );
    assert_eq!(
        manager
            .load_token(&TokenScope::Shared, "github")
            .expect("load shared token"),
        None
    );

    let path = state_dir.join("auth/users/alice/github.token");
    assert!(
        path.exists(),
        "the token must live under the user's directory"
    );
    let mode = fs::metadata(&path)
        .expect("token metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "a stored token must be 0600");
    for dir in ["auth/users", "auth/users/alice"] {
        let mode = fs::metadata(state_dir.join(dir))
            .expect("namespace directory metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{dir} must be 0700 whatever the umask is");
    }

    let _ = fs::remove_dir_all(state_dir);
}

#[test]
fn a_workspace_owner_selects_the_namespace_its_token_comes_from() {
    let state_dir = temp_state_dir("owner-resolution");
    let manager = AuthManager::new(&state_dir);
    manager
        .store_token(
            &TokenScope::User("alice".to_string()),
            "github",
            "alice-token",
            false,
        )
        .expect("store alice's token");
    manager
        .store_token(&TokenScope::Shared, "github", "shared-token", false)
        .expect("store the shared token");
    let providers = vec!["github".to_string()];

    let owned = manager.resolve_tokens(Some("alice"), &providers);
    assert_eq!(owned.len(), 1);
    assert_eq!(owned[0].token, "alice-token");
    assert_eq!(owned[0].env_var, "GITHUB_TOKEN");

    // An owner with nothing stored gets nothing, rather than falling back to the
    // shared token: the fallback would hand one user another's credential.
    let other = manager.resolve_tokens(Some("bob"), &providers);
    assert!(other.is_empty(), "bob must not receive a token");

    // No owner keeps the behaviour every workspace had before namespaces.
    let legacy = manager.resolve_tokens(None, &providers);
    assert_eq!(legacy.len(), 1);
    assert_eq!(legacy[0].token, "shared-token");

    let _ = fs::remove_dir_all(state_dir);
}

#[test]
fn storing_over_an_existing_token_needs_force() {
    let state_dir = temp_state_dir("overwrite");
    let manager = AuthManager::new(&state_dir);
    let alice = TokenScope::User("alice".to_string());

    assert!(manager
        .store_token(&alice, "github", "first", false)
        .expect("store the first token")
        .stored());

    let second = manager
        .store_token(&alice, "github", "second", false)
        .expect("a refused overwrite is not an error");
    assert!(!second.stored(), "an existing token must not be replaced");
    assert_eq!(
        manager
            .load_token(&alice, "github")
            .expect("load token")
            .as_deref(),
        Some("first")
    );

    let forced = manager
        .store_token(&alice, "github", "second", true)
        .expect("force replaces the token");
    assert!(forced.stored());
    assert_eq!(
        manager
            .load_token(&alice, "github")
            .expect("load token")
            .as_deref(),
        Some("second")
    );

    let _ = fs::remove_dir_all(state_dir);
}

#[test]
fn a_user_namespace_directory_that_is_not_private_is_refused() {
    let state_dir = temp_state_dir("insecure-dir");
    let manager = AuthManager::new(&state_dir);
    let alice = TokenScope::User("alice".to_string());
    manager
        .store_token(&alice, "github", "alice-token", false)
        .expect("store a token");

    let user_dir = state_dir.join("auth/users/alice");
    fs::set_permissions(&user_dir, fs::Permissions::from_mode(0o755)).expect("widen the directory");

    assert!(
        manager.load_token(&alice, "github").is_err(),
        "a namespace directory anyone can enter must not be read from"
    );

    let _ = fs::remove_dir_all(state_dir);
}
