//! The workspace list, one workspace's status, and its published ports.

use super::format::{column_width, format_published_port, workspace_status_label};
use anyhow::Result;

use super::*;

pub(in crate::commands::workspace) fn print_workspace_list_by_sandbox(
    workspaces: Vec<WorkspaceListItem>,
) -> Result<()> {
    if workspaces.is_empty() {
        println!("no workspaces");
        return Ok(());
    }
    let id_width = column_width(&workspaces, |w| w.id.len(), "WORKSPACE_ID".len());
    let name_width = column_width(&workspaces, |w| w.name.len(), "NAME".len());
    let status_width = column_width(
        &workspaces,
        |w| format!("{:?}", w.status).to_lowercase().len(),
        "STATUS".len(),
    );
    println!(
        "{:<id_width$} {:<name_width$} {:<status_width$}",
        "WORKSPACE_ID", "NAME", "STATUS"
    );
    for workspace in workspaces {
        println!(
            "{:<id_width$} {:<name_width$} {:<status_width$}",
            workspace.id,
            workspace.name,
            format!("{:?}", workspace.status).to_lowercase()
        );
    }
    Ok(())
}

pub(in crate::commands::workspace) fn print_workspace_list_all(
    workspaces: Vec<WorkspaceMetadata>,
) -> Result<()> {
    if workspaces.is_empty() {
        println!("no workspaces");
        return Ok(());
    }

    let workspace_id_width = column_width(&workspaces, |w| w.id.len(), "WORKSPACE_ID".len());
    let sandbox_id_width = column_width(&workspaces, |w| w.sandbox_id.len(), "SANDBOX_ID".len());
    let name_width = column_width(&workspaces, |w| w.name.len(), "NAME".len());

    println!(
        "{:<workspace_id_width$} {:<sandbox_id_width$} {:<name_width$} CREATED",
        "WORKSPACE_ID", "SANDBOX_ID", "NAME"
    );
    for workspace in workspaces {
        println!(
            "{:<workspace_id_width$} {:<sandbox_id_width$} {:<name_width$} {}",
            workspace.id, workspace.sandbox_id, workspace.name, workspace.created_at
        );
    }
    Ok(())
}

pub(in crate::commands::workspace) fn print_workspace_status(status: &WorkspaceStatusReport) {
    println!("id: {}", status.id);
    println!("name: {}", status.name);
    println!("created_at: {}", status.created_at);
    println!("allocated_path: {}", status.allocated_path);
    println!("status: {}", workspace_status_label(&status.status));
    println!("active_process_count: {}", status.active_process_count);
    print_workspace_limits("workspace_limits", &status.limits);
    print_sandbox_limits("sandbox_limits", &status.sandbox_limits);
    if let Some(resource_usage) = &status.resource_usage {
        println!("resource_usage: {}", resource_usage);
    }
    if !status.published_ports.is_empty() {
        println!("published_ports:");
        for port in &status.published_ports {
            println!("  {}", format_published_port(port));
        }
    }
}

pub(in crate::commands::workspace) fn print_workspace_ports(ports: &[PublishedPortStatus]) {
    if ports.is_empty() {
        println!("no published ports");
        return;
    }

    for port in ports {
        println!("{}", format_published_port(port));
    }
}

fn print_workspace_limits(label: &str, limits: &crate::workspace::WorkspaceLimits) {
    println!(
        "{label}: cpu_seconds={}, cpu_percent={}, memory_bytes={}, max_processes={}, max_open_files={}, disk_bytes={}",
        limits
            .cpu_seconds
            .map_or_else(|| "unlimited".to_string(), |v| v.to_string()),
        limits
            .cpu_percent
            .map(crate::resource_limits::format_cpu_percent)
            .unwrap_or_else(|| "unlimited".to_string()),
        limits
            .memory_bytes
            .map_or_else(|| "unlimited".to_string(), |v| v.to_string()),
        limits
            .max_processes
            .map_or_else(|| "unlimited".to_string(), |v| v.to_string()),
        limits
            .max_open_files
            .map_or_else(|| "unlimited".to_string(), |v| v.to_string()),
        limits
            .disk_bytes
            .map_or_else(|| "unlimited".to_string(), |v| v.to_string()),
    );
}

fn print_sandbox_limits(label: &str, limits: &crate::sandbox::SandboxLimits) {
    println!(
        "{label}: cpu_percent={}, memory_bytes={}, max_processes={}",
        limits
            .cpu_percent
            .map(crate::resource_limits::format_cpu_percent)
            .unwrap_or_else(|| "unlimited".to_string()),
        limits
            .memory_bytes
            .map_or_else(|| "unlimited".to_string(), |v| v.to_string()),
        limits
            .max_processes
            .map_or_else(|| "unlimited".to_string(), |v| v.to_string()),
    );
}
