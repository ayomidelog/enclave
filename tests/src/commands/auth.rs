use super::format_auth_provider_list;

#[test]
fn auth_provider_list_shows_supported_and_configured_providers() {
    let output = format_auth_provider_list(&["github".to_string()]);
    assert!(output.contains("Supported auth providers:"));
    assert!(output.contains("- enclave"));
    assert!(output.contains("- github (configured)"));
    assert!(output.contains("- npm"));
}
