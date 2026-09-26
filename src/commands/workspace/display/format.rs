//! Shared value formatters for the workspace printers.

use super::*;

pub(super) fn format_published_port(port: &PublishedPortStatus) -> String {
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

pub(super) fn format_percent(value: f64) -> String {
    format!("{value:.2}%")
}

pub(super) fn format_cpu_limit(workspace_limit: Option<f64>, sandbox_limit: Option<f64>) -> String {
    let workspace = workspace_limit
        .map(crate::resource_limits::format_cpu_percent)
        .unwrap_or_else(|| "unlimited".to_string());
    let sandbox = sandbox_limit
        .map(crate::resource_limits::format_cpu_percent)
        .unwrap_or_else(|| "unlimited".to_string());
    format!("workspace={} sandbox={}", workspace, sandbox)
}

pub(super) fn format_optional_bytes(value: Option<u64>) -> String {
    value.map(format_bytes).unwrap_or_else(|| "n/a".to_string())
}

pub(super) fn format_bytes(value: u64) -> String {
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

pub(super) fn column_width<T>(
    items: &[T],
    len_fn: impl Fn(&T) -> usize,
    min_width: usize,
) -> usize {
    items
        .iter()
        .map(&len_fn)
        .max()
        .unwrap_or(min_width)
        .max(min_width)
}

/// The lowercase label used in list and status output.
pub(super) fn workspace_status_label(status: &WorkspaceStatus) -> &'static str {
    status.as_str()
}
