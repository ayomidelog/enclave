use super::authorize::{is_policy_exempt, POLICY_EXEMPT_ACTIONS};
use super::decision::evaluate_policy_decision;
use crate::policy::types::Policy;
use crate::policy::types::PolicyRule;

#[test]
fn uid_specific_deny_overrides_wildcard_allow() {
    let policy = Policy {
        default_allow: false,
        rules: vec![
            PolicyRule {
                uid: None,
                allow: vec!["sandbox.*".to_string()],
                deny: vec![],
            },
            PolicyRule {
                uid: Some(1000),
                allow: vec![],
                deny: vec!["sandbox.destroy".to_string()],
            },
        ],
        ..Policy::default()
    };

    let decision = evaluate_policy_decision(&policy, 1000, "sandbox.destroy");
    assert_eq!(decision, Some(false));
}

#[test]
fn deny_precedence_within_rule_group() {
    let policy = Policy {
        default_allow: false,
        rules: vec![PolicyRule {
            uid: Some(1000),
            allow: vec!["workspace.*".to_string()],
            deny: vec!["workspace.destroy".to_string()],
        }],
        ..Policy::default()
    };

    assert_eq!(
        evaluate_policy_decision(&policy, 1000, "workspace.destroy"),
        Some(false)
    );
    assert_eq!(
        evaluate_policy_decision(&policy, 1000, "workspace.status"),
        Some(true)
    );
}

#[test]
fn default_deny_without_matching_rules() {
    let policy = Policy {
        default_allow: false,
        rules: vec![],
        ..Policy::default()
    };

    assert_eq!(
        evaluate_policy_decision(&policy, 1000, "sandbox.list"),
        None
    );
}

/// Only an action that reads state or decides the policy may skip authorization.
///
/// The exempt list is the whole of the policy engine's blind spot: an action on it
/// runs for any local user with no rule consulted. The assertion is on the exact
/// contents rather than on a sample, so a name added to the list fails this test
/// until someone writes down why that action does not change host state.
#[test]
fn only_read_only_and_self_referential_actions_are_exempt_from_policy() {
    assert_eq!(
        POLICY_EXEMPT_ACTIONS,
        &[
            "ping",
            "shutdown",
            "daemon.health",
            "policy.get",
            "policy.set_default",
            "policy.allow",
            "policy.deny",
            "policy.clear",
        ],
        "an action was added to or removed from the policy exempt list; every entry \
         has to read state or decide the policy, and nothing that changes host state \
         may be here"
    );
}

/// An action that changes host state must not be exempt.
///
/// The names are the protocol strings a client sends, which are a stable contract,
/// so this does not depend on the daemon's action enum being visible here.
#[test]
fn actions_that_change_host_state_are_authorized() {
    for action in [
        "init",
        "sandbox.create",
        "sandbox.start",
        "sandbox.stop",
        "sandbox.pause",
        "sandbox.resume",
        "sandbox.destroy",
        "sandbox.wipe",
        "sandbox.remove",
        "sandbox.exec_setup",
        "workspace.create",
        "workspace.start",
        "workspace.start_many",
        "workspace.stop",
        "workspace.destroy",
        "workspace.wipe",
        "workspace.remove",
        "workspace.update",
        "workspace.resize",
        "workspace.exec",
        "workspace.cp",
        "workspace.port.publish",
        "workspace.port.unpublish",
        "workspace.snapshot",
        "workspace.restore",
        "workspace.snapshot.gc",
        "workspace.snapshot.export",
        "workspace.snapshot.import",
        "registry.repair",
        "daemon.doctor.repair",
    ] {
        assert!(
            !is_policy_exempt(action),
            "{action} changes host state and must be authorized, not exempt"
        );
    }
}
