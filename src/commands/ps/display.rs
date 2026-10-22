use crate::workspace::ps::format_uptime;
use crate::workspace::ProcessEntry;

use crate::commands::table::column_width;

pub(super) fn print_process_table(entries: &[ProcessEntry]) {
    if entries.is_empty() {
        println!("no workspaces");
        return;
    }

    let sandbox_width = column_width(entries, |e| e.sandbox.len(), "SANDBOX".len());
    let workspace_width = column_width(entries, |e| e.workspace.len(), "WORKSPACE".len());
    let status_width = column_width(entries, |e| e.status.len(), "STATUS".len());
    let uptime_width = column_width(
        entries,
        |e| format_uptime_cell(e.uptime).len(),
        "UPTIME".len(),
    );
    let pid_width = column_width(entries, |e| format_pid_cell(e.pid).len(), "PID".len());

    println!(
        "{:<sandbox_width$}  {:<workspace_width$}  {:<status_width$}  {:<uptime_width$}  {:<pid_width$}",
        "SANDBOX", "WORKSPACE", "STATUS", "UPTIME", "PID"
    );

    for entry in entries {
        println!(
            "{:<sandbox_width$}  {:<workspace_width$}  {:<status_width$}  {:<uptime_width$}  {:<pid_width$}",
            entry.sandbox,
            entry.workspace,
            entry.status,
            format_uptime_cell(entry.uptime),
            format_pid_cell(entry.pid),
        );
    }
}

pub(super) fn print_process_table_with_processes(entries: &[ProcessEntry]) {
    print_process_table(entries);
    for entry in entries {
        if entry.processes.is_empty() {
            continue;
        }
        println!();
        println!("{} / {}", entry.sandbox, entry.workspace);
        println!("  PID      PPID     PGID     AGE       CPU%   RSS        THREADS  DETACHED  JOB  COMMAND");
        for process in &entry.processes {
            println!(
                "  {:<8} {:<8} {:<8} {:<9} {:<6} {:<10} {:<8} {:<9} {:<4} {}",
                process.pid,
                process.ppid,
                process.process_group,
                process
                    .age_secs
                    .map(format_uptime)
                    .unwrap_or_else(|| "-".to_string()),
                process
                    .cpu_percent
                    .map(|value| format!("{value:.1}"))
                    .unwrap_or_else(|| "-".to_string()),
                process
                    .rss_bytes
                    .map(|value| format!("{:.1}M", value as f64 / 1024.0 / 1024.0))
                    .unwrap_or_else(|| "-".to_string()),
                process
                    .threads
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                if process.detached { "yes" } else { "no" },
                process.job_id.as_deref().unwrap_or("-"),
                process.command,
            );
        }
    }
}

fn format_uptime_cell(uptime: Option<u64>) -> String {
    match uptime {
        Some(secs) => format_uptime(secs),
        None => "\u{2014}".to_string(),
    }
}

fn format_pid_cell(pid: Option<u32>) -> String {
    match pid {
        Some(p) => p.to_string(),
        None => "\u{2014}".to_string(),
    }
}

#[cfg(test)]
#[path = "../../../tests/src/commands/ps/display.rs"]
mod tests;
