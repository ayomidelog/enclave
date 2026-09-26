//! Reading a sandbox, and the commands that act on all of them.
//!
//! `list` and `status` are read-only. `remove` deletes a record the registry no
//! longer needs, and `wipe` deletes every sandbox at once, so both confirm first.
//!
//! Wipe confirms against the count it just read rather than against a static
//! phrase, because the phrase is the operator's only chance to notice that the
//! command is about to delete more than they expected. The report is printed
//! before the command fails, so a partial wipe names every outcome instead of
//! only the error.

use std::path::Path;

use anyhow::{bail, Result};
use serde_json::json;

use crate::cli::WipeArgs;
use crate::sandbox::{RootfsTier, SandboxListItem, SandboxStatusReport};

use super::super::{
    confirm_destructive_action, daemon, print_operation_id, print_state_transition,
    report_retained_resources, require_confirmation, send, send_managed,
};

/// What `sandbox.wipe` removed, failed on, and had to leave behind.
#[derive(serde::Deserialize)]
struct SandboxWipeReport {
    #[serde(default)]
    removed: Vec<String>,
    #[serde(default)]
    errors: Vec<String>,
    #[serde(default)]
    retained: std::collections::BTreeMap<String, Vec<crate::workspace::RetainedResource>>,
}

pub(crate) fn run_list(socket: &Path) -> Result<()> {
    let response = send_managed(socket, "sandbox.list", json!({}))?;
    let sandboxes: Vec<SandboxListItem> = serde_json::from_value(response)?;

    if sandboxes.is_empty() {
        println!("no sandboxes");
        return Ok(());
    }

    // Column widths come from the data, so the table stays aligned for any set of
    // names without a fixed layout.
    let id_width = sandboxes
        .iter()
        .map(|s| s.id.len())
        .max()
        .unwrap_or(2)
        .max("ID".len());
    let name_width = sandboxes
        .iter()
        .map(|s| s.name.len())
        .max()
        .unwrap_or(4)
        .max("NAME".len());
    let status_width = sandboxes
        .iter()
        .map(|s| format!("{:?}", s.status).to_lowercase().len())
        .max()
        .unwrap_or(6)
        .max("STATUS".len());

    println!(
        "{:<id_width$} {:<name_width$} {:<status_width$} WORKSPACES",
        "ID", "NAME", "STATUS"
    );
    for sandbox in sandboxes {
        println!(
            "{:<id_width$} {:<name_width$} {:<status_width$} {}",
            sandbox.id,
            sandbox.name,
            format!("{:?}", sandbox.status).to_lowercase(),
            sandbox.workspace_count
        );
    }

    Ok(())
}

pub(crate) fn run_status(socket: &Path, sandbox: &str) -> Result<()> {
    let response = send_managed(socket, "sandbox.status", json!({ "sandbox": sandbox }))?;
    let status: SandboxStatusReport = serde_json::from_value(response)?;
    println!("id: {}", status.id);
    println!("name: {}", status.name);
    println!("created_at: {}", status.created_at);
    println!("status: {}", format!("{:?}", status.status).to_lowercase());
    println!("rootfs_path: {}", status.rootfs_path);
    // The backend is what decides what creating this sandbox cost, and it is the one
    // property of a sandbox a caller cannot read off the other fields: a shared overlay
    // and a private copy have the same shape on disk. The base it shares is named
    // beside it, because that is the directory an operator has to look at to tell two
    // sandboxes on the shared tier apart.
    println!("rootfs_tier: {}", rootfs_tier(&status));
    println!(
        "rootfs_disk_usage_bytes: {}",
        status.rootfs_disk_usage_bytes
    );
    println!("workspace_count: {}", status.workspace_count);
    if let Some(cpu_percent) = status.limits.cpu_percent {
        println!(
            "cpu_limit: {}",
            crate::resource_limits::format_cpu_percent(cpu_percent)
        );
    }
    println!(
        "memory_limit_bytes: {}",
        status
            .limits
            .memory_bytes
            .map_or_else(|| "unlimited".to_string(), |v| v.to_string())
    );
    println!(
        "max_processes: {}",
        status
            .limits
            .max_processes
            .map_or_else(|| "unlimited".to_string(), |v| v.to_string())
    );
    Ok(())
}

/// The base-image backend, with what it means for the lifecycle.
///
/// The shared tier mounts the cached rootfs as an immutable lower layer, so creating
/// a sandbox does not copy the tree and no write ever reaches the shared base. The
/// copied tier is the fallback for a host where that overlay cannot be set up, and it
/// pays a full recursive copy once, at creation.
fn rootfs_tier(status: &SandboxStatusReport) -> String {
    match status.rootfs_tier {
        RootfsTier::SharedOverlay => format!(
            "shared_overlay (OverlayFS over {}, which is never copied and never written)",
            status.rootfs_lower_path.as_deref().unwrap_or("unknown")
        ),
        RootfsTier::Copied => {
            "copied (a private copy of the rootfs, made once at creation)".to_string()
        }
    }
}

pub(crate) fn run_remove(socket: &Path, sandbox_id: &str) -> Result<()> {
    tracing::info!("removing sandbox '{}'...", sandbox_id);
    let response = send_managed(
        socket,
        "sandbox.remove",
        json!({ "sandbox_id": sandbox_id }),
    )?;
    println!("removed sandbox '{}'", sandbox_id);
    print_state_transition(&response);
    print_operation_id();
    Ok(())
}

pub(crate) fn run_wipe(socket: &Path, args: WipeArgs) -> Result<()> {
    daemon::ensure_daemon_running_for_action(socket, "sandbox.wipe")?;
    let response = send(socket, "sandbox.list", json!({}))?;
    let sandboxes: Vec<SandboxListItem> = serde_json::from_value(response)?;
    if sandboxes.is_empty() {
        println!("no sandboxes");
        return Ok(());
    }

    require_confirmation(
        confirm_destructive_action(
            &format!(
                "this will permanently delete all {} sandboxes and their workspaces.{}",
                sandboxes.len(),
                if args.force {
                    " force mode removes the records even when host resources cannot be released."
                } else {
                    ""
                }
            ),
            "delete all sandboxes",
        )?,
        "wipe",
        "Run it from a terminal, or destroy the sandboxes one at a time with `enclave destroy`.",
    )?;

    let response = send_managed(socket, "sandbox.wipe", json!({ "force": args.force }))?;
    let report: SandboxWipeReport = serde_json::from_value(response)?;
    println!(
        "deleted {} sandboxes; {} failed",
        report.removed.len(),
        report.errors.len()
    );
    report_retained_resources(report.retained.iter().flat_map(|(sandbox, resources)| {
        resources
            .iter()
            .map(move |item| format!("{sandbox}/{}: {}", item.resource, item.detail))
    }));
    if !report.errors.is_empty() {
        bail!(
            "sandbox wipe completed with {} error(s)",
            report.errors.len()
        );
    }
    Ok(())
}
