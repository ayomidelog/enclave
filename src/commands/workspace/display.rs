use anyhow::Result;

use crate::workspace::{
    PublishedPortState, PublishedPortStatus, WorkspaceListItem, WorkspaceMetadata,
    WorkspaceSnapshotInfo, WorkspaceStatsReport, WorkspaceStatus, WorkspaceStatusReport,
};

pub(super) fn print_workspace_list_by_sandbox(workspaces: Vec<WorkspaceListItem>) -> Result<()> {
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

pub(super) fn print_workspace_list_all(workspaces: Vec<WorkspaceMetadata>) -> Result<()> {
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

pub(super) fn print_workspace_status(status: &WorkspaceStatusReport) {
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

pub(super) fn print_workspace_ports(ports: &[PublishedPortStatus]) {
    if ports.is_empty() {
        println!("no published ports");
        return;
    }

    for port in ports {
        println!("{}", format_published_port(port));
    }
}

pub(crate) fn print_workspace_stats(status: &WorkspaceStatsReport) {
    println!("sandbox: {}", status.sandbox_id);
    println!("workspace: {}", status.name);
    println!("workspace_id: {}", status.id);
    println!("status: {}", workspace_status_label(&status.status));
    println!(
        "pid: {}",
        status
            .pid
            .map_or_else(|| "unavailable".to_string(), |v| v.to_string())
    );
    println!(
        "cpu: {}",
        status
            .cpu_percent
            .map(format_percent)
            .unwrap_or_else(|| "unavailable".to_string())
    );
    println!(
        "cpu_limit: {}",
        format_cpu_limit(status.cpu_limit_percent, status.sandbox_cpu_limit_percent)
    );
    println!(
        "memory: {} / {}",
        format_optional_bytes(status.memory_usage_bytes),
        format_optional_bytes(status.memory_limit_bytes)
    );
    println!(
        "sandbox_memory_limit: {}",
        format_optional_bytes(status.sandbox_memory_limit_bytes)
    );
    println!(
        "memory_percent: {}",
        status
            .memory_percent
            .map(format_percent)
            .unwrap_or_else(|| "unlimited".to_string())
    );
    println!(
        "network_io: rx={} tx={}",
        format_optional_bytes(status.net_rx_bytes),
        format_optional_bytes(status.net_tx_bytes)
    );
    println!(
        "block_io: read={} write={}",
        format_optional_bytes(status.block_read_bytes),
        format_optional_bytes(status.block_write_bytes)
    );
    println!(
        "pids: {} / {}",
        status.pids_current.map_or_else(
            || status.active_process_count.to_string(),
            |v| v.to_string()
        ),
        status
            .pids_limit
            .map_or_else(|| "unlimited".to_string(), |v| v.to_string())
    );
    println!(
        "threads: {}",
        status
            .threads
            .map_or_else(|| "unavailable".to_string(), |v| v.to_string())
    );
    println!("active_process_count: {}", status.active_process_count);
}

pub(crate) fn print_workspace_stats_table(stats: &[WorkspaceStatsReport]) -> Result<()> {
    if stats.is_empty() {
        println!("no running workspaces");
        return Ok(());
    }

    let sandbox_width = column_width(stats, |s| s.sandbox_id.len(), "SANDBOX".len());
    let workspace_width = column_width(stats, |s| s.name.len(), "WORKSPACE".len());
    let cpu_width = column_width(
        stats,
        |s| {
            s.cpu_percent
                .map(format_percent)
                .unwrap_or_else(|| "n/a".to_string())
                .len()
        },
        "CPU %".len(),
    );
    let mem_width = column_width(
        stats,
        |s| {
            format!(
                "{} / {}",
                format_optional_bytes(s.memory_usage_bytes),
                format_optional_bytes(s.memory_limit_bytes)
            )
            .len()
        },
        "MEM USAGE / LIMIT".len(),
    );
    let mem_pct_width = column_width(
        stats,
        |s| {
            s.memory_percent
                .map(format_percent)
                .unwrap_or_else(|| "n/a".to_string())
                .len()
        },
        "MEM %".len(),
    );
    let net_width = column_width(
        stats,
        |s| {
            format!(
                "{} / {}",
                format_optional_bytes(s.net_rx_bytes),
                format_optional_bytes(s.net_tx_bytes)
            )
            .len()
        },
        "NET I/O".len(),
    );
    let block_width = column_width(
        stats,
        |s| {
            format!(
                "{} / {}",
                format_optional_bytes(s.block_read_bytes),
                format_optional_bytes(s.block_write_bytes)
            )
            .len()
        },
        "BLOCK I/O".len(),
    );
    let pids_width = column_width(
        stats,
        |s| {
            format!(
                "{} / {}",
                s.pids_current
                    .map_or_else(|| s.active_process_count.to_string(), |v| v.to_string()),
                s.pids_limit
                    .map_or_else(|| "∞".to_string(), |v| v.to_string())
            )
            .len()
        },
        "PIDS".len(),
    );

    println!(
        "{:<sandbox_width$} {:<workspace_width$} {:>cpu_width$} {:>mem_width$} {:>mem_pct_width$} {:>net_width$} {:>block_width$} {:>pids_width$}",
        "SANDBOX",
        "WORKSPACE",
        "CPU %",
        "MEM USAGE / LIMIT",
        "MEM %",
        "NET I/O",
        "BLOCK I/O",
        "PIDS"
    );
    for stat in stats {
        println!(
            "{:<sandbox_width$} {:<workspace_width$} {:>cpu_width$} {:>mem_width$} {:>mem_pct_width$} {:>net_width$} {:>block_width$} {:>pids_width$}",
            stat.sandbox_id,
            stat.name,
            stat.cpu_percent
                .map(format_percent)
                .unwrap_or_else(|| "n/a".to_string()),
            format!(
                "{} / {}",
                format_optional_bytes(stat.memory_usage_bytes),
                format_optional_bytes(stat.memory_limit_bytes)
            ),
            stat.memory_percent
                .map(format_percent)
                .unwrap_or_else(|| "n/a".to_string()),
            format!(
                "{} / {}",
                format_optional_bytes(stat.net_rx_bytes),
                format_optional_bytes(stat.net_tx_bytes)
            ),
            format!(
                "{} / {}",
                format_optional_bytes(stat.block_read_bytes),
                format_optional_bytes(stat.block_write_bytes)
            ),
            format!(
                "{} / {}",
                stat.pids_current
                    .map_or_else(|| stat.active_process_count.to_string(), |v| v.to_string()),
                stat.pids_limit
                    .map_or_else(|| "∞".to_string(), |v| v.to_string())
            ),
        );
    }
    Ok(())
}

fn workspace_status_label(status: &WorkspaceStatus) -> &'static str {
    match status {
        WorkspaceStatus::Running => "running",
        WorkspaceStatus::Stopped => "stopped",
    }
}

fn print_workspace_limits(label: &str, limits: &crate::workspace::WorkspaceLimits) {
    println!(
        "{label}: cpu_seconds={}, cpu_percent={}, memory_bytes={}, max_processes={}, max_open_files={}",
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

fn format_published_port(port: &PublishedPortStatus) -> String {
    let target = match (&port.state, port.workspace_ip.as_deref()) {
        (PublishedPortState::Active, Some(workspace_ip)) => {
            format!("{workspace_ip}:{}", port.workspace_port)
        }
        _ => format!("workspace:{}", port.workspace_port),
    };

    match port.state {
        PublishedPortState::Active => format!(
            "{}:{} -> {}/{}",
            port.host_ip, port.host_port, target, port.protocol
        ),
        PublishedPortState::Configured => format!(
            "{}:{} -> {}/{} [configured]",
            port.host_ip, port.host_port, target, port.protocol
        ),
        PublishedPortState::Failed => format!(
            "{}:{} -> {}/{} [failed: {}]",
            port.host_ip,
            port.host_port,
            target,
            port.protocol,
            port.error.as_deref().unwrap_or("unknown error")
        ),
    }
}

fn format_percent(value: f64) -> String {
    format!("{value:.2}%")
}

fn format_cpu_limit(workspace_limit: Option<f64>, sandbox_limit: Option<f64>) -> String {
    let workspace = workspace_limit
        .map(crate::resource_limits::format_cpu_percent)
        .unwrap_or_else(|| "unlimited".to_string());
    let sandbox = sandbox_limit
        .map(crate::resource_limits::format_cpu_percent)
        .unwrap_or_else(|| "unlimited".to_string());
    format!("workspace={} sandbox={}", workspace, sandbox)
}

fn format_optional_bytes(value: Option<u64>) -> String {
    value.map(format_bytes).unwrap_or_else(|| "n/a".to_string())
}

fn format_bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = value as f64;
    let mut unit = 0usize;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

pub(super) fn print_snapshot_list(snapshots: Vec<WorkspaceSnapshotInfo>) -> Result<()> {
    if snapshots.is_empty() {
        println!("no snapshots");
        return Ok(());
    }

    let name_width = column_width(&snapshots, |s| s.name.len(), "NAME".len());
    let created_width = column_width(&snapshots, |s| s.created_at.len(), "CREATED_AT".len());
    println!(
        "{:<name_width$} {:<created_width$} PATH",
        "NAME", "CREATED_AT"
    );
    for snapshot in snapshots {
        println!(
            "{:<name_width$} {:<created_width$} {}",
            snapshot.name, snapshot.created_at, snapshot.path
        );
    }
    Ok(())
}

fn column_width<T>(items: &[T], len_fn: impl Fn(&T) -> usize, min_width: usize) -> usize {
    items
        .iter()
        .map(&len_fn)
        .max()
        .unwrap_or(min_width)
        .max(min_width)
}

#[cfg(test)]
#[path = "../../../tests/src/commands/workspace/display.rs"]
mod tests;
