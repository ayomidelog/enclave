use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn workspace_name_validation_rejects_dots() {
    assert!(validate_name("project_1").is_ok());
    assert!(validate_name("project.name").is_err());
    assert!(validate_name("..").is_err());
}

#[test]
fn resolve_home_mount_source_accepts_existing_absolute_directory() {
    let dir = std::env::temp_dir().join(format!("enclave-mount-source-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create temp mount dir");
    let expected = dir
        .canonicalize()
        .expect("canonicalize temp mount dir")
        .to_string_lossy()
        .to_string();
    let resolved = resolve_home_mount_source(Some(dir.to_string_lossy().as_ref()))
        .expect("resolve mount source");
    assert_eq!(resolved.as_deref(), Some(expected.as_str()));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn resolve_home_mount_source_rejects_relative_directory() {
    assert!(resolve_home_mount_source(Some("relative/path")).is_err());
}

#[test]
fn ensure_traversable_directory_permissions_sets_mode_755() {
    let dir = std::env::temp_dir().join(format!(
        "enclave-traversable-dir-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("set initial perms");

    ensure_traversable_directory_permissions(&dir).expect("normalize permissions");

    let mode = fs::metadata(&dir)
        .expect("read metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o755);
    fs::remove_dir_all(dir).expect("cleanup temp dir");
}

#[test]
fn normalize_env_tokens_trims_uppercases_and_deduplicates() {
    let tokens = normalize_env_tokens(vec![
        " enclave_token ".to_string(),
        "ENCLAVE_TOKEN".to_string(),
    ])
    .expect("normalize env tokens");
    assert_eq!(tokens, vec!["ENCLAVE_TOKEN".to_string()]);
}

/// A workspace may ask for a credential the provider table has never heard of.
///
/// The name is the variable it wants, and the value comes from the store slot
/// derived from it, so anything the store can be given a name for is a name a
/// workspace may ask for.
#[test]
fn normalize_env_tokens_accepts_a_name_that_is_not_a_provider() {
    let tokens = normalize_env_tokens(vec![
        "netflix_password".to_string(),
        "GTBANK_CARD_NUMBER".to_string(),
        "_internal".to_string(),
    ])
    .expect("normalize env tokens");
    assert_eq!(
        tokens,
        vec![
            "GTBANK_CARD_NUMBER".to_string(),
            "NETFLIX_PASSWORD".to_string(),
            "_INTERNAL".to_string()
        ]
    );
}

/// A name that could never be written into the workspace is refused where it is
/// declared, rather than stored and silently never injected.
#[test]
fn normalize_env_tokens_rejects_a_name_it_could_never_inject() {
    for invalid in [
        "NETFLIX-PASSWORD",
        "1TOKEN",
        "TOKEN NAME",
        "TOKEN=X",
        "TOKEN_$X",
    ] {
        assert!(
            normalize_env_tokens(vec![invalid.to_string()]).is_err(),
            "{invalid:?} must be refused"
        );
    }
}
