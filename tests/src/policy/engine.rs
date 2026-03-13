use super::*;
use crate::policy::types::Policy;

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
