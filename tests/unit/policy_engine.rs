use std::fs;
use std::path::PathBuf;

use enclave::policy::{add_allow_rule, add_deny_rule, authorize, clear_rules, set_default_allow};

fn state_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create state dir");
    dir
}

#[test]
fn allow_rule_grants_access() {
    let state = state_dir("enclave-policy-allow");
    set_default_allow(&state, false).expect("set default");
    add_allow_rule(&state, Some(1001), "workspace.exec").expect("add allow");

    authorize(&state, 1001, "workspace.exec").expect("allow rule should grant access");
    let _ = fs::remove_dir_all(state);
}

#[test]
fn deny_rule_overrides_allow() {
    let state = state_dir("enclave-policy-deny");
    set_default_allow(&state, true).expect("set default");
    add_allow_rule(&state, Some(1002), "workspace.*").expect("add allow");
    add_deny_rule(&state, Some(1002), "workspace.destroy").expect("add deny");

    let denied = authorize(&state, 1002, "workspace.destroy");
    assert!(denied.is_err());
    let _ = fs::remove_dir_all(state);
}

#[test]
fn uid_specific_rule_overrides_wildcard_rule() {
    let state = state_dir("enclave-policy-uid-override");
    set_default_allow(&state, false).expect("set default");
    add_allow_rule(&state, None, "sandbox.*").expect("add wildcard allow");
    add_deny_rule(&state, Some(2000), "sandbox.destroy").expect("add uid deny");

    assert!(authorize(&state, 2000, "sandbox.destroy").is_err());
    authorize(&state, 3000, "sandbox.destroy").expect("wildcard allow should permit other uid");
    let _ = fs::remove_dir_all(state);
}

#[test]
fn wildcard_matching_applies() {
    let state = state_dir("enclave-policy-wildcard");
    set_default_allow(&state, false).expect("set default");
    clear_rules(&state, None).expect("clear rules");
    add_allow_rule(&state, Some(1003), "workspace.*").expect("add wildcard allow");

    authorize(&state, 1003, "workspace.exec").expect("wildcard should match exec");
    authorize(&state, 1003, "workspace.logs").expect("wildcard should match logs");
    let _ = fs::remove_dir_all(state);
}
