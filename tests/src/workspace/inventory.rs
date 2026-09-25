use super::*;

/// A workspace with no runtime and no address, so the inventory can only contain
/// what the test puts on disk.
fn idle_workspace(dir: &std::path::Path) -> WorkspaceMetadata {
    WorkspaceMetadata {
        id: "ws-inventory".to_string(),
        sandbox_id: "sb-inventory".to_string(),
        name: "ws-inventory".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        workspace_path: dir.to_string_lossy().to_string(),
        filesystem_path: dir.join("fs").to_string_lossy().to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: String::new(),
        overlay_home_base_path: String::new(),
        overlay_home_upper_path: String::new(),
        overlay_home_work_path: String::new(),
        overlay_home_merged_path: String::new(),
        auth_providers: Vec::new(),
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: Default::default(),
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: Default::default(),
        clear_tmp_on_restart: false,
        limits: Default::default(),
        assigned_ip: None,
    }
}

fn fixture(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "enclave-inventory-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("runtime")).expect("create workspace dir");
    dir
}

#[test]
fn an_idle_workspace_owns_nothing() {
    let dir = fixture("idle");
    let workspace = idle_workspace(&dir);
    let inventory = ResourceInventory::collect("sb-inventory", &workspace);
    assert!(inventory.is_empty(), "{:?}", inventory.resources);
    assert_eq!(
        inventory.surviving_summary(),
        "all 0 recorded resource(s) released"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_live_runtime_is_recorded_with_its_start_time() {
    // The start time is part of the identity because a pid on its own matches
    // whatever process holds the number later. The current process is the one live
    // process a unit test can name with certainty.
    let dir = fixture("runtime");
    let mut workspace = idle_workspace(&dir);
    let pid = std::process::id();
    let starttime = crate::workspace::session_process_matches(pid, None)
        .then(|| {
            crate::workspace::session_for_tests::process_starttime_ticks(pid).expect("start time")
        })
        .expect("the current process is alive");
    workspace.runtime_pid = Some(pid);
    workspace.runtime_starttime_ticks = Some(starttime);

    let inventory = ResourceInventory::collect("sb-inventory", &workspace);
    assert_eq!(inventory.count(ResourceKind::Process), 1);
    assert_eq!(inventory.surviving().len(), 1);
    let described = inventory.surviving_summary();
    assert!(described.contains(&pid.to_string()), "{described}");
    assert!(described.contains(&starttime.to_string()), "{described}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stale_runtime_identity_is_not_recorded() {
    // A pid that is not running owns nothing, so it must not appear in the
    // inventory: recording it would make the later diff report a resource that
    // never existed.
    let dir = fixture("stale");
    let mut workspace = idle_workspace(&dir);
    workspace.runtime_pid = Some(u32::MAX);
    workspace.runtime_starttime_ticks = Some(1);
    let inventory = ResourceInventory::collect("sb-inventory", &workspace);
    assert_eq!(inventory.count(ResourceKind::Process), 0);
    assert!(inventory.is_empty(), "{:?}", inventory.resources);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn runtime_markers_are_recorded_and_their_removal_is_proven() {
    // The pid file, the ready file, and the namespace references are what a later
    // start reads, so a stop that leaves one behind is a real failure. Recording
    // them is what lets the certificate prove they are gone.
    let dir = fixture("markers");
    let workspace = idle_workspace(&dir);
    let pid_file = crate::workspace::session_for_tests::runtime_pid_file(&workspace);
    let ready_file = crate::workspace::session_for_tests::runtime_ready_file(&workspace);
    std::fs::write(&pid_file, "1234\n").expect("write pid file");
    std::fs::write(&ready_file, "ready\n").expect("write ready file");

    let inventory = ResourceInventory::collect("sb-inventory", &workspace);
    assert_eq!(
        inventory.count(ResourceKind::Path),
        2,
        "{:?}",
        inventory.resources
    );
    assert_eq!(inventory.surviving().len(), 2);

    // Removing them makes the same inventory prove the release.
    std::fs::remove_file(&pid_file).expect("remove pid file");
    std::fs::remove_file(&ready_file).expect("remove ready file");
    assert!(inventory.surviving().is_empty());
    assert!(inventory
        .surviving_summary()
        .contains("all 2 recorded resource(s) released"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_surviving_summary_names_each_resource_by_kind() {
    let dir = fixture("summary");
    let workspace = idle_workspace(&dir);
    std::fs::write(
        crate::workspace::session_for_tests::runtime_pid_file(&workspace),
        "1234\n",
    )
    .expect("write pid file");

    let inventory = ResourceInventory::collect("sb-inventory", &workspace);
    let summary = inventory.surviving_summary();
    assert!(
        summary.starts_with("1 of 1 recorded resource(s) survived"),
        "{summary}"
    );
    assert!(summary.contains("file "), "{summary}");
    assert!(summary.contains("session.pid"), "{summary}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resource_kinds_have_stable_report_labels() {
    assert_eq!(ResourceKind::Process.as_str(), "process");
    assert_eq!(ResourceKind::LoopDevice.as_str(), "loop_device");
    assert_eq!(ResourceKind::FirewallRule.as_str(), "firewall_rule");
}
