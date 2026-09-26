//! The labels the CLI prints for a workspace.

use super::*;

use crate::workspace::WorkspaceLimits;

fn status_with_disk(disk_bytes: Option<u64>) -> WorkspaceStatusReport {
    WorkspaceStatusReport {
        id: "ws-1".to_string(),
        name: "dev".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        allocated_path: "/state/sandboxes/sb/workspaces/ws-1".to_string(),
        status: crate::workspace::WorkspaceStatus::Running,
        active_process_count: 1,
        resource_usage: None,
        limits: WorkspaceLimits {
            disk_bytes,
            ..WorkspaceLimits::default()
        },
        sandbox_limits: Default::default(),
        published_ports: Vec::new(),
    }
}

/// The status line must name the storage tier, because it is what decides what a start
/// and a stop cost and it is not derivable from the other fields a reader sees.
#[test]
fn the_storage_tier_names_the_tier_and_its_lifecycle_cost() {
    let quota = storage_tier(&status_with_disk(Some(512 * 1024 * 1024)));
    assert!(quota.starts_with("quota"), "{quota}");
    assert!(quota.contains("loop device"), "{quota}");
    assert!(
        quota.contains("536870912"),
        "the label must state the allocation it is describing: {quota}"
    );
    assert!(quota.contains("boot"), "{quota}");

    let directory = storage_tier(&status_with_disk(None));
    assert!(directory.starts_with("directory"), "{directory}");
    assert!(directory.contains("overlay"), "{directory}");
    assert!(directory.contains("boot"), "{directory}");
}
