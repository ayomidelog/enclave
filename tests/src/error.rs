use super::*;

#[test]
fn a_coded_error_reports_its_code_through_anyhow() {
    let error = coded(ErrorCode::NotFound, "sandbox 'x' not found");
    assert_eq!(code_of(&error), ErrorCode::NotFound);
    assert_eq!(error.to_string(), "sandbox 'x' not found");
}

#[test]
fn context_preserves_the_code() {
    // The lifecycle layers add context as an error travels outward, which must
    // not lose the category the raising site attached.
    let error = coded(
        ErrorCode::Conflict,
        "another operation holds this workspace",
    )
    .context("failed to start workspace 'w'");
    assert_eq!(code_of(&error), ErrorCode::Conflict);
    assert!(error.to_string().contains("failed to start workspace 'w'"));
}

#[test]
fn an_uncategorized_error_is_internal() {
    let error = anyhow::anyhow!("something went wrong");
    assert_eq!(code_of(&error), ErrorCode::Internal);
}

#[test]
fn every_code_has_a_stable_name() {
    // The name is what a client matches on, so it is part of the protocol and
    // must not change silently.
    for (code, name) in [
        (ErrorCode::InvalidRequest, "invalid_request"),
        (ErrorCode::NotFound, "not_found"),
        (ErrorCode::Conflict, "conflict"),
        (ErrorCode::RateLimited, "rate_limited"),
        (ErrorCode::PolicyDenied, "policy_denied"),
        (ErrorCode::Unsupported, "unsupported"),
        (ErrorCode::Timeout, "timeout"),
        (ErrorCode::CleanupIncomplete, "cleanup_incomplete"),
        (ErrorCode::Internal, "internal"),
    ] {
        assert_eq!(code.as_str(), name);
        let encoded = serde_json::to_string(&code).expect("serialize the code");
        assert_eq!(encoded, format!("\"{name}\""));
    }
}
