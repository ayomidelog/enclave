use crate::workspace::ps::format_uptime;
use crate::workspace::ProcessEntry;

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

fn column_width<T>(items: &[T], len_fn: impl Fn(&T) -> usize, min_width: usize) -> usize {
    items
        .iter()
        .map(&len_fn)
        .max()
        .unwrap_or(min_width)
        .max(min_width)
}

#[cfg(test)]
#[path = "../../../tests/src/commands/ps/display.rs"]
mod tests;
