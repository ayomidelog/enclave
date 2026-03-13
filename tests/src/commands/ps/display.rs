use super::*;

#[test]
fn format_uptime_cell_none_shows_dash() {
    let cell = format_uptime_cell(None);
    assert_eq!(cell, "\u{2014}");
}

#[test]
fn format_uptime_cell_some_shows_time() {
    let cell = format_uptime_cell(Some(8040));
    assert_eq!(cell, "2h 14m");
}

#[test]
fn format_pid_cell_none_shows_dash() {
    let cell = format_pid_cell(None);
    assert_eq!(cell, "\u{2014}");
}

#[test]
fn format_pid_cell_some_shows_number() {
    let cell = format_pid_cell(Some(84921));
    assert_eq!(cell, "84921");
}

#[test]
fn column_width_uses_minimum() {
    let items: Vec<ProcessEntry> = vec![];
    assert_eq!(column_width(&items, |e| e.sandbox.len(), 10), 10);
}

#[test]
fn column_width_uses_max_item() {
    let entries = vec![
        ProcessEntry {
            sandbox: "short".to_string(),
            workspace: "ws".to_string(),
            status: "running".to_string(),
            uptime: None,
            pid: None,
        },
        ProcessEntry {
            sandbox: "much-longer-sandbox-name".to_string(),
            workspace: "ws".to_string(),
            status: "stopped".to_string(),
            uptime: None,
            pid: None,
        },
    ];
    assert_eq!(
        column_width(&entries, |e| e.sandbox.len(), 5),
        "much-longer-sandbox-name".len()
    );
}
