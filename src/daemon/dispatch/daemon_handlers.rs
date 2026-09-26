//! The daemon's own requests: health, doctor, init, and shutdown.
//!
//! These act on the daemon rather than on a sandbox or a workspace, so they take
//! the services and the shutdown flag instead of a selector.

use super::*;

pub(super) fn dispatch_daemon_health(
    config: &DaemonConfig,
    services: &crate::daemon::services::DaemonServices,
) -> Result<Value> {
    Ok(json!({
        "status": "ok",
        "pid": std::process::id(),
        "state_dir": config.state_dir.to_string_lossy(),
        "socket_path": config.socket_path.to_string_lossy(),
        // Lifecycle operations running right now, so a slow daemon can be
        // explained without reading the log.
        "active_operations": services
            .active_operations
            .in_flight()
            .iter()
            .map(crate::daemon::active_operations::ActiveOperation::describe)
            .collect::<Vec<_>>(),
        // The last lifecycle operation the daemon ran, so a status report can
        // name it without the operator reading the journal directory.
        "last_operation": crate::operation::latest(&config.state_dir)?,
        // The deadlines actually in force, so a wait that ended early or late
        // can be explained without reading the environment of the daemon.
        "deadlines": crate::deadlines::describe_all(),
        "metrics": crate::perf::metrics(),
    }))
}

pub(super) fn dispatch_daemon_doctor(
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
    services: &crate::daemon::services::DaemonServices,
) -> Result<Value> {
    // The daemon owns the port publisher and the running operations, so this is
    // the one caller that can report a published listener no running workspace is
    // using and tell a live operation apart from one whose daemon died.
    let daemon = crate::doctor::DaemonState {
        port_publisher,
        active_operations: services.active_operations.as_ref(),
    };
    let report = crate::doctor::run_doctor(&config.state_dir, Some(&daemon))?;
    Ok(serde_json::to_value(report)?)
}

pub(super) fn dispatch_daemon_doctor_repair(config: &DaemonConfig) -> Result<Value> {
    let report = crate::doctor::repair_doctor(&config.state_dir, &config.socket_path)?;
    Ok(serde_json::to_value(report)?)
}

pub(super) fn dispatch_init(config: &DaemonConfig) -> Result<Value> {
    sandbox::init_storage(&config.state_dir)?;
    Ok(json!({
        "state_dir": config.state_dir.to_string_lossy(),
        "socket_path": config.socket_path.to_string_lossy(),
    }))
}

/// Ask the daemon to stop serving.
///
/// The flag is what the accept loop and the workers read. The wake is what makes
/// the loop notice without waiting for its poll interval: this runs on a worker
/// thread, so no signal is delivered to the loop.
pub(super) fn dispatch_shutdown(shutdown: &Arc<AtomicBool>) -> Result<Value> {
    shutdown.store(true, Ordering::SeqCst);
    crate::daemon::shutdown::SIGNAL_SHUTDOWN.store(true, Ordering::SeqCst);
    crate::daemon::shutdown::wake_shutdown_wait();
    Ok(json!({"status": "shutting_down"}))
}
