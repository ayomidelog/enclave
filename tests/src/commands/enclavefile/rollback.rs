use super::failed_up_rollback_target;
use crate::sandbox::SandboxStatus;

#[test]
fn failed_up_rolls_back_new_and_stopped_sandboxes() {
    assert_eq!(
        failed_up_rollback_target(None, false),
        Some(SandboxStatus::Stopped)
    );
    assert_eq!(
        failed_up_rollback_target(Some(&SandboxStatus::Stopped), false),
        Some(SandboxStatus::Stopped)
    );
}

#[test]
fn failed_up_restores_paused_sandboxes_and_preserves_running_ones() {
    assert_eq!(
        failed_up_rollback_target(Some(&SandboxStatus::Paused), false),
        Some(SandboxStatus::Paused)
    );
    assert_eq!(
        failed_up_rollback_target(Some(&SandboxStatus::Running), false),
        None
    );
}

#[test]
fn failed_rebuild_rolls_back_to_stopped() {
    assert_eq!(
        failed_up_rollback_target(Some(&SandboxStatus::Running), true),
        Some(SandboxStatus::Stopped)
    );
}
