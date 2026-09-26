//! Which copy of a workspace record wins when the two disagree.
//!
//! A workspace is described twice, in the registry and in its own directory, and the
//! precedence rule is that the registry is authoritative and repair adopts the disk copy
//! rather than silently overwriting it. This is the test for that rule.

use super::*;
/// The workspace metadata the on-disk copy and the registry record are built from.
fn disagreement_workspace(
    sandbox: &crate::sandbox::SandboxMetadata,
    workspace_dir: &std::path::Path,
) -> crate::workspace::WorkspaceMetadata {
    crate::workspace::WorkspaceMetadata {
        id: "ws-disagree".to_string(),
        sandbox_id: sandbox.id.clone(),
        name: "disagree".to_string(),
        created_at: "2026-08-06T00:00:00Z".to_string(),
        workspace_path: workspace_dir.to_string_lossy().to_string(),
        filesystem_path: workspace_dir.join("fs").to_string_lossy().to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: sandbox.rootfs_path.clone(),
        overlay_home_base_path: sandbox.home_base_path.clone(),
        overlay_home_upper_path: workspace_dir
            .join("home-upper")
            .to_string_lossy()
            .to_string(),
        overlay_home_work_path: workspace_dir
            .join("home-work")
            .to_string_lossy()
            .to_string(),
        overlay_home_merged_path: workspace_dir
            .join("home-merged")
            .to_string_lossy()
            .to_string(),
        auth_providers: Vec::new(),
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: crate::workspace::WorkspaceStatus::Stopped,
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: Default::default(),
        clear_tmp_on_restart: false,
        limits: crate::workspace::WorkspaceLimits::default(),
        assigned_ip: None,
    }
}

/// A lifecycle step writes the per-directory metadata before it commits the
/// registry record, so when the two disagree the file is the newer copy. Repair
/// therefore adopts it, and reports the disagreement rather than overwriting the
/// record silently: an operator looking at a workspace whose state changed
/// unexpectedly needs to see that the copies had diverged and which one won.
#[test]
fn repair_reports_a_metadata_disagreement_and_adopts_the_disk_copy() {
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-registry-disagreement-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = fs::remove_dir_all(&state_dir);

    let sandbox_dir = state_dir.join("sandboxes").join("sb-disagree");
    let workspace_dir = sandbox_dir.join("workspaces").join("ws-disagree");
    fs::create_dir_all(workspace_dir.join("fs")).expect("create workspace dir");
    fs::create_dir_all(sandbox_dir.join("rootfs")).expect("create rootfs dir");

    let sandbox = crate::sandbox::SandboxMetadata {
        id: "sb-disagree".to_string(),
        name: "sb-disagree".to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: crate::sandbox::BootstrapMethod::CachedRootfs,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        sandbox_path: sandbox_dir.to_string_lossy().to_string(),
        rootfs_path: sandbox_dir.join("rootfs").to_string_lossy().to_string(),
        rootfs_lower_path: None,
        mounted_rootfs_path: sandbox_dir
            .join("runtime/rootfs.mnt")
            .to_string_lossy()
            .to_string(),
        workspaces_path: sandbox_dir.join("workspaces").to_string_lossy().to_string(),
        home_base_path: sandbox_dir.join("home-base").to_string_lossy().to_string(),
        limits: crate::sandbox::SandboxLimits::default(),
        status: crate::sandbox::SandboxStatus::Stopped,
    };
    fs::write(
        sandbox_dir.join("sandbox.json"),
        serde_json::to_string_pretty(&sandbox).expect("serialize sandbox metadata"),
    )
    .expect("write sandbox metadata");

    // The on-disk copy says the workspace is running with a recorded runtime.
    let mut on_disk = disagreement_workspace(&sandbox, &workspace_dir);
    on_disk.status = crate::workspace::WorkspaceStatus::Running;
    on_disk.runtime_pid = Some(4242);
    on_disk.runtime_starttime_ticks = Some(999);
    on_disk.assigned_ip = Some("10.200.0.10".to_string());
    fs::write(
        workspace_dir.join("workspace.json"),
        serde_json::to_string_pretty(&on_disk).expect("serialize workspace metadata"),
    )
    .expect("write workspace metadata");

    // The registry record says the same workspace is stopped: they disagree.
    let mut in_registry = on_disk.clone();
    in_registry.status = crate::workspace::WorkspaceStatus::Stopped;
    in_registry.runtime_pid = None;
    in_registry.runtime_starttime_ticks = None;
    in_registry.assigned_ip = None;
    with_registry_mut(&state_dir, |registry| {
        registry.sandboxes.insert(
            sandbox.id.clone(),
            RegistrySandbox {
                metadata: sandbox.clone(),
                workspaces: std::collections::BTreeMap::from([(
                    in_registry.id.clone(),
                    in_registry,
                )]),
            },
        );
        Ok(())
    })
    .expect("seed the registry record");

    let report = repair_registry(&state_dir, false).expect("repair should succeed");

    assert_eq!(
        report.metadata_disagreements.len(),
        1,
        "{:?}",
        report.metadata_disagreements
    );
    let disagreement = &report.metadata_disagreements[0];
    assert_eq!(disagreement.sandbox_id, "sb-disagree");
    assert_eq!(disagreement.workspace_id.as_deref(), Some("ws-disagree"));
    assert_eq!(disagreement.adopted, "disk");
    for field in ["status", "runtime_pid", "assigned_ip"] {
        assert!(
            disagreement
                .differences
                .iter()
                .any(|difference| difference.starts_with(field)),
            "expected {field} among {:?}",
            disagreement.differences
        );
    }

    // The on-disk copy won, which is the rule the report makes visible.
    with_registry(&state_dir, |registry| {
        let workspace = &registry.sandboxes["sb-disagree"].workspaces["ws-disagree"];
        assert_eq!(workspace.status, crate::workspace::WorkspaceStatus::Running);
        assert_eq!(workspace.runtime_pid, Some(4242));
        assert_eq!(workspace.assigned_ip.as_deref(), Some("10.200.0.10"));
        Ok(())
    })
    .expect("read the repaired registry");

    let _ = fs::remove_dir_all(&state_dir);
}
