use super::*;
use std::fs;

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
