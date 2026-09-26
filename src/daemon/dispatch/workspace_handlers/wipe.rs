//! Wiping every workspace on the host.
//!
//! A wipe spans sandboxes, so it is the one workspace request that takes no
//! selector. It reports the port publisher's listeners for each workspace it
//! removed, because the publisher is the daemon's and the workspace layer cannot
//! reach it.

use super::*;

pub(in crate::daemon::dispatch) fn dispatch_workspace_wipe(
    params: &Value,
    config: &DaemonConfig,
    port_publisher: &Arc<PortPublisher>,
) -> Result<Value> {
    let mode = parse_cleanup_mode(params)?;
    let workspaces = workspace::list_workspaces(&config.state_dir, None)?;
    let report = workspace::destroy_all_workspaces(&config.state_dir, mode)?;
    for workspace in &workspaces {
        if report.removed.contains(&workspace.id) {
            port_publisher.clear_workspace_ports(&workspace.sandbox_id, &workspace.id);
        }
    }
    if !report.errors.is_empty() {
        bail!(
            "workspace wipe completed with {} error(s): {}",
            report.errors.len(),
            report.errors.join("; ")
        );
    }
    Ok(serde_json::to_value(report)?)
}
