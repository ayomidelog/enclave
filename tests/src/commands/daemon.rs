use super::*;

#[test]
fn destructive_actions_require_explicit_daemon_start() {
    for action in [
        "sandbox.destroy",
        "sandbox.wipe",
        "workspace.destroy",
        "workspace.wipe",
        "registry.repair",
        "daemon.doctor.repair",
    ] {
        assert!(
            destructive_action_requires_explicit_start(action),
            "{action}"
        );
    }
    assert!(!destructive_action_requires_explicit_start("sandbox.list"));
    assert!(!destructive_action_requires_explicit_start(
        "workspace.start"
    ));
}
