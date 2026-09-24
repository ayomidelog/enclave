//! The per-workspace lifecycle operations: start, stop, destroy, status, stats.
//!
//! They share a parameter shape, so they share one entry point and are selected
//! by name. The daemon owns host resources that the workspace layer cannot see,
//! so the port publisher is folded into each outcome here.

use super::*;

pub(in crate::daemon::dispatch) fn dispatch_workspace_target(
    params: &Value,
    config: &DaemonConfig,
    operation: &str,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let sandbox = require_param_str(params, &["sandbox", "sandbox_id"])?;
    let workspace_selector = require_param_str(params, &["workspace", "workspace_id", "name"])?;

    match operation {
        "start" => {
            let previous_state =
                workspace_state_before(&config.state_dir, sandbox, workspace_selector);
            let metadata = workspace::start_workspace_with_security(
                &config.state_dir,
                sandbox,
                workspace_selector,
                config.workspace_apparmor_profile.as_deref(),
                config.workspace_selinux_label.as_deref(),
            )?;
            let metadata =
                ensure_workspace_ports_started(&config.state_dir, &metadata, port_publisher)?;
            Ok(with_transition(
                serde_json::to_value(metadata)?,
                previous_state,
                WorkspaceStatus::Running.as_str(),
            ))
        }
        "stop" => {
            let previous_state =
                workspace_state_before(&config.state_dir, sandbox, workspace_selector);
            let (metadata, certificate) = workspace::stop_workspace_with_certificate(
                &config.state_dir,
                sandbox,
                workspace_selector,
            )?;
            port_publisher.clear_workspace_ports(&metadata.sandbox_id, &metadata.id);
            // The port publisher lives in the daemon, so its release is verified
            // here and folded into the workspace cleanup certificate.
            let released =
                !port_publisher.has_active_workspace_ports(&metadata.sandbox_id, &metadata.id);
            let certificate = certificate.with_ports_released(released);
            if !certificate.is_complete() {
                bail!(
                    "workspace '{}' stop left resources behind: {}",
                    metadata.id,
                    certificate.failure_summary()
                );
            }
            Ok(with_transition(
                json!({ "workspace": metadata, "certificate": certificate }),
                previous_state,
                WorkspaceStatus::Stopped.as_str(),
            ))
        }
        "destroy" => {
            let metadata_before =
                workspace::workspace_metadata(&config.state_dir, sandbox, workspace_selector)?;
            let previous_state = metadata_before.status.as_str();
            let mode = parse_cleanup_mode(params)?;
            let report = workspace::destroy_workspace_with_mode(
                &config.state_dir,
                sandbox,
                workspace_selector,
                mode,
            )?;
            port_publisher.clear_workspace_ports(&metadata_before.sandbox_id, &metadata_before.id);
            // The port publisher lives in the daemon, so its release is verified
            // here and folded into the destroy certificate, exactly as the stop
            // path does.
            let released = !port_publisher
                .has_active_workspace_ports(&metadata_before.sandbox_id, &metadata_before.id);
            let certificate = report.certificate.with_ports_released(released);
            // The registry record is already removed by this point, so a listener
            // that would not close cannot be turned back into a failure without
            // misreporting what happened. It is reported instead: the certificate
            // names it and the CLI points the operator at doctor repair.
            if !certificate.is_complete() {
                tracing::warn!(
                    "workspace '{}' destroy left resources behind: {}",
                    report.workspace_id,
                    certificate.failure_summary()
                );
            }
            Ok(with_transition(
                json!({
                    "workspace_id": report.workspace_id,
                    "mode": report.mode,
                    "retained": report.retained,
                    "certificate": certificate,
                    "sandbox": sandbox,
                }),
                previous_state,
                ABSENT,
            ))
        }
        "status" => {
            let metadata =
                workspace::workspace_metadata(&config.state_dir, sandbox, workspace_selector)?;
            let report = workspace::workspace_status(
                &config.state_dir,
                sandbox,
                workspace_selector,
                &port_publisher.workspace_statuses(&metadata.sandbox_id, &metadata.id),
            )?;
            Ok(serde_json::to_value(report)?)
        }
        "stats" => {
            let report =
                workspace::workspace_stats(&config.state_dir, sandbox, workspace_selector)?;
            Ok(serde_json::to_value(report)?)
        }
        "remove" => {
            let metadata_before =
                workspace::workspace_metadata(&config.state_dir, sandbox, workspace_selector)?;
            let previous_state = metadata_before.status.as_str();
            workspace::remove_workspace(&config.state_dir, sandbox, workspace_selector)?;
            port_publisher.clear_workspace_ports(&metadata_before.sandbox_id, &metadata_before.id);
            Ok(with_transition(
                json!({
                    "removed": workspace_selector,
                    "sandbox": sandbox,
                }),
                previous_state,
                ABSENT,
            ))
        }
        "runtime" => {
            let result =
                workspace::workspace_runtime_info(&config.state_dir, sandbox, workspace_selector)?;
            Ok(serde_json::to_value(result)?)
        }
        "snapshot_list" => {
            let result = workspace::list_workspace_snapshots(
                &config.state_dir,
                sandbox,
                workspace_selector,
            )?;
            Ok(serde_json::to_value(result)?)
        }
        _ => bail!("unknown workspace target operation '{}'", operation),
    }
}
