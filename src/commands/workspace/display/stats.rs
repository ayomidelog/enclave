//! The live workspace stats table.

use super::format::{
    column_width, format_cpu_limit, format_optional_bytes, format_percent, workspace_status_label,
};
use anyhow::Result;

use super::*;

pub(in crate::commands::workspace) fn print_workspace_stats(status: &WorkspaceStatsReport) {
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
