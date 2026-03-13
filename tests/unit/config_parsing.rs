use enclave::enclavefile::parse_enclavefile;
use enclave::sandbox::BootstrapMethod;

#[test]
fn parses_enclavefile_workspace_and_bootstrap_method() {
    let enclavefile = parse_enclavefile(
        r#"
[sandbox]
name = "devbox"
bootstrap_method = "cached_rootfs"

[workspace.api]
name = "api"
run = "node server.js"
path = "/tmp/api"
auth = ["github", "npm"]
env_tokens = ["ENCLAVE_TOKEN"]
"#,
    )
    .expect("enclavefile should parse");

    assert_eq!(
        enclavefile.sandbox.bootstrap_method,
        BootstrapMethod::CachedRootfs
    );
    let workspace = enclavefile.workspace.get("api").expect("workspace exists");
    assert_eq!(workspace.name, "api");
    assert_eq!(workspace.run.as_deref(), Some("node server.js"));
    assert_eq!(workspace.path.as_deref(), Some("/tmp/api"));
    assert_eq!(
        workspace.auth,
        vec!["github".to_string(), "npm".to_string()]
    );
    assert_eq!(workspace.env_tokens, vec!["ENCLAVE_TOKEN".to_string()]);
}

#[test]
fn rejects_invalid_bootstrap_method() {
    let result = parse_enclavefile(
        r#"
[sandbox]
name = "devbox"
bootstrap_method = "invalid_method"
"#,
    );

    assert!(result.is_err());
}

#[test]
fn rejects_unknown_workspace_auth_provider() {
    let result = parse_enclavefile(
        r#"
[sandbox]
name = "devbox"

[workspace.api]
name = "api"
auth = ["gitlab"]
"#,
    );
    assert!(result.is_err());
}

#[test]
fn accepts_trimmed_mixed_case_workspace_auth_provider() {
    let result = parse_enclavefile(
        r#"
[sandbox]
name = "devbox"

[workspace.api]
name = "api"
auth = [" GitHub "]
"#,
    );

    assert!(result.is_ok());
}

#[test]
fn accepts_mixed_case_workspace_env_token_and_preserves_original_value() {
    let result = parse_enclavefile(
        r#"
[sandbox]
name = "devbox"

[workspace.api]
name = "api"
env_tokens = [" enclave_token "]
"#,
    );

    let enclavefile = result.expect("enclavefile should parse");
    let workspace = enclavefile.workspace.get("api").expect("workspace exists");
    assert_eq!(workspace.env_tokens, vec![" enclave_token ".to_string()]);
}

#[test]
fn rejects_unknown_workspace_env_token() {
    let result = parse_enclavefile(
        r#"
[sandbox]
name = "devbox"

[workspace.api]
name = "api"
env_tokens = ["SECRET_API_KEY"]
"#,
    );
    assert!(result.is_err());
}

#[test]
fn parses_workspace_dir_field() {
    let enclavefile = parse_enclavefile(
        r#"
[sandbox]
name = "devbox"

[workspace.api]
name = "api"
workspace_dir = "./project"
"#,
    )
    .expect("enclavefile should parse");

    let workspace = enclavefile.workspace.get("api").expect("workspace exists");
    assert_eq!(workspace.workspace_dir.as_deref(), Some("./project"));
}

#[test]
fn rejects_workspace_with_both_path_and_workspace_dir() {
    let result = parse_enclavefile(
        r#"
[sandbox]
name = "devbox"

[workspace.api]
name = "api"
path = "/tmp/api"
workspace_dir = "./project"
"#,
    );
    assert!(result.is_err());
}
