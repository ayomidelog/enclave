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

/// The audit log names what happened and never the credential it happened to.
///
/// The log is the one place a token operation leaves a durable trace, so it is
/// also the place a secret could most easily end up somewhere it should not. The
/// check is on the file's bytes rather than on a parsed field, because a value
/// reaching the log through a field nobody thought about is exactly the failure
/// this is meant to catch.
#[test]
fn the_audit_log_records_the_event_without_the_token_value() {
    let state_dir = temp_state_dir("audit");
    let manager = AuthManager::new(&state_dir);
    let alice = TokenScope::User("alice".to_string());
    let secret = "ghp_AuditProbeValue_9f3a1c";

    manager
        .store_token(&alice, "github", secret, false)
        .expect("store the token");
    manager
        .delete_token(&alice, "github")
        .expect("remove the token");

    let log_path = state_dir.join("auth/audit.log");
    let log = fs::read_to_string(&log_path).expect("read the audit log");
    assert!(
        !log.contains(secret),
        "the audit log must never contain the token value"
    );

    let events: Vec<serde_json::Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).expect("every audit line is json"))
        .collect();
    assert_eq!(events.len(), 2, "one line per event: {log}");
    assert_eq!(events[0]["action"], "store");
    assert_eq!(events[0]["user"], "alice");
    assert_eq!(events[0]["provider"], "github");
    assert_eq!(events[1]["action"], "revoke");

    let mode = fs::metadata(&log_path)
        .expect("audit log metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "the audit log must not be readable by others");

    let _ = fs::remove_dir_all(state_dir);
}

/// A store that is refused is not an event, so it is not recorded.
///
/// The log is a record of what happened to a credential. A refused overwrite did
/// not change one, and recording it would make the log claim a store that never
/// occurred.
#[test]
fn a_refused_store_writes_no_audit_line() {
    let state_dir = temp_state_dir("audit-refused");
    let manager = AuthManager::new(&state_dir);
    let alice = TokenScope::User("alice".to_string());

    manager
        .store_token(&alice, "github", "first", false)
        .expect("store the first token");
    let refused = manager
        .store_token(&alice, "github", "second", false)
        .expect("a refused overwrite is not an error");
    assert!(!refused.stored());

    let log = fs::read_to_string(state_dir.join("auth/audit.log")).expect("read the audit log");
    assert_eq!(
        log.lines().count(),
        1,
        "only the store that happened: {log}"
    );

    let _ = fs::remove_dir_all(state_dir);
}

/// A provider declared both as `auth` and as an `env_token` is one credential.
///
/// Both declarations export the same value from the same token file, so resolving
/// them separately would scrub and record the same secret twice. The dedup is what
/// keeps the audit honest: one credential, one event.
#[test]
fn a_provider_declared_both_ways_is_one_credential() {
    let state_dir = temp_state_dir("dedup");
    let manager = AuthManager::new(&state_dir);
    manager
        .store_token(
            &TokenScope::User("alice".to_string()),
            "github",
            "shared-value",
            false,
        )
        .expect("store the token");

    let credentials = manager.resolve_workspace_credentials(
        Some("alice"),
        &["github".to_string()],
        &["GITHUB_TOKEN".to_string()],
    );
    assert_eq!(
        credentials.len(),
        1,
        "the same provider declared twice is one credential: {credentials:?}"
    );
    assert_eq!(credentials[0].name, "github");
    assert_eq!(credentials[0].token, "shared-value");

    let _ = fs::remove_dir_all(state_dir);
}

/// An environment token is resolved even when no `auth` provider is declared.
///
/// It puts the same credential in the workspace, so it is scrubbed and recorded
/// the same way. Missing it would leave the one declaration a workspace can make
/// that holds a secret out of both the scrubber and the audit.
#[test]
fn an_environment_token_alone_is_resolved() {
    let state_dir = temp_state_dir("env-only");
    let manager = AuthManager::new(&state_dir);
    manager
        .store_token(
            &TokenScope::User("alice".to_string()),
            "enclave",
            "env-only-value",
            false,
        )
        .expect("store the token");

    let credentials =
        manager.resolve_workspace_credentials(Some("alice"), &[], &["ENCLAVE_TOKEN".to_string()]);
    assert_eq!(credentials.len(), 1);
    assert_eq!(credentials[0].name, "enclave");
    assert_eq!(credentials[0].token, "env-only-value");

    let _ = fs::remove_dir_all(state_dir);
}

/// A credential the provider table does not contain is stored under a slot
/// derived from the variable name, and that is where the workspace reads it.
#[test]
fn a_free_form_environment_token_resolves_the_slot_its_name_derives() {
    let state_dir = temp_state_dir("free-form");
    let manager = AuthManager::new(&state_dir);
    let alice = TokenScope::User("alice".to_string());
    manager
        .store_token(&alice, "netflix-password", "hunter2", false)
        .expect("store a slot the provider table does not contain");

    let credentials = manager.resolve_workspace_credentials(
        Some("alice"),
        &[],
        &["NETFLIX_PASSWORD".to_string()],
    );
    assert_eq!(
        credentials.len(),
        1,
        "the token must resolve: {credentials:?}"
    );
    assert_eq!(credentials[0].name, "netflix-password");
    assert_eq!(credentials[0].env_var, "NETFLIX_PASSWORD");
    assert_eq!(credentials[0].token, "hunter2");

    let _ = fs::remove_dir_all(state_dir);
}

/// The same isolation a provider token has, for a slot the table does not
/// contain: one user's vault entry is not another user's.
#[test]
fn a_free_form_environment_token_is_scoped_to_its_owner() {
    let state_dir = temp_state_dir("free-form-owner");
    let manager = AuthManager::new(&state_dir);
    manager
        .store_token(
            &TokenScope::User("alice".to_string()),
            "netflix-password",
            "alice-password",
            false,
        )
        .expect("store alice's slot");
    let env_tokens = vec!["NETFLIX_PASSWORD".to_string()];

    let alice_credentials = manager.resolve_workspace_credentials(Some("alice"), &[], &env_tokens);
    assert_eq!(alice_credentials.len(), 1);
    assert_eq!(alice_credentials[0].token, "alice-password");

    assert!(
        manager
            .resolve_workspace_credentials(Some("bob"), &[], &env_tokens)
            .is_empty(),
        "bob must not receive alice's credential"
    );
    assert!(
        manager
            .resolve_workspace_credentials(None, &[], &env_tokens)
            .is_empty(),
        "the shared namespace must not fall back to a user's credential"
    );

    let _ = fs::remove_dir_all(state_dir);
}

/// A name that is one of the providers' variables keeps resolving to that
/// provider, which is what every workspace that declared one before free-form
/// names existed relies on.
#[test]
fn a_provider_variable_in_env_tokens_still_reads_the_provider_slot() {
    let state_dir = temp_state_dir("provider-env-token");
    let manager = AuthManager::new(&state_dir);
    manager
        .store_token(&TokenScope::Shared, "github", "ghp_legacy", false)
        .expect("store the provider token");

    let credentials =
        manager.resolve_workspace_credentials(None, &[], &["GITHUB_TOKEN".to_string()]);
    assert_eq!(credentials.len(), 1);
    assert_eq!(credentials[0].name, "github");
    assert_eq!(credentials[0].token, "ghp_legacy");

    let _ = fs::remove_dir_all(state_dir);
}
