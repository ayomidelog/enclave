//! Rolling a failed `up` back to the state the sandbox was in before it.
//!
//! An `up` that fails partway leaves the sandbox somewhere between the state it
//! started in and the state it was asked for. Reporting the failure is not
//! enough: the caller needs the sandbox put back, or told exactly why it could
//! not be.

use super::*;

pub(super) fn failed_up_rollback_target(
    initial_status: Option<&SandboxStatus>,
    rebuilding: bool,
) -> Option<SandboxStatus> {
    if rebuilding || initial_status.is_none() {
        return Some(SandboxStatus::Stopped);
    }
    match initial_status.expect("checked above") {
        SandboxStatus::Running => None,
        SandboxStatus::Paused => Some(SandboxStatus::Paused),
        SandboxStatus::Stopped => Some(SandboxStatus::Stopped),
        // An interrupted transition is resolved by rolling back to stopped,
        // matching how the daemon reconciles transitional sandboxes.
        SandboxStatus::Starting | SandboxStatus::Stopping => Some(SandboxStatus::Stopped),
    }
}

pub(super) fn rollback_failed_up(
    socket: &Path,
    sandbox_name: &str,
    target_status: Option<SandboxStatus>,
    cause: anyhow::Error,
) -> anyhow::Error {
    let Some(target_status) = target_status else {
        return cause;
    };
    let current_status = match sandbox_status_by_name(socket, sandbox_name) {
        Ok(Some(status)) => status,
        Ok(None) => return cause,
        Err(error) => {
            return anyhow::anyhow!(
                "environment setup failed: {cause:#}; could not inspect sandbox '{}' for rollback: {error:#}",
                sandbox_name
            );
        }
    };
    if current_status == target_status {
        return cause;
    }

    let (action, description) = match target_status {
        SandboxStatus::Stopped => ("sandbox.stop", "stop"),
        SandboxStatus::Paused => ("sandbox.pause", "pause"),
        // `failed_up_rollback_target` never returns these, so reaching them
        // means the sandbox is already where it needs to be.
        SandboxStatus::Running | SandboxStatus::Starting | SandboxStatus::Stopping => return cause,
    };
    if let Err(error) = send(socket, action, json!({ "sandbox": sandbox_name })) {
        return anyhow::anyhow!(
            "environment setup failed: {cause:#}; sandbox '{}' remains {:?} because rollback could not {description} it: {error:#}",
            sandbox_name,
            current_status
        );
    }
    cause.context(format!(
        "sandbox '{}' was rolled back to {:?} after setup failed",
        sandbox_name, target_status
    ))
}

#[cfg(test)]
mod tests {
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
}
