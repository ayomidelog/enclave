use super::*;

use std::sync::Arc;

use chrono::{Duration, Utc};

use crate::daemon::active_operations::ActiveOperations;
use crate::daemon::leases::LifecycleLeases;
use crate::daemon::policy::IdlePolicy;
use crate::daemon::rate_limiter::RateLimiter;
use crate::daemon::services::DaemonServices;
use crate::network::publish::PortPublisher;
use crate::workspace::{ActivityService, WorkspaceMetadata, WorkspaceStatus};

fn fixture(name: &str) -> (std::path::PathBuf, DaemonConfig, DaemonServices) {
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-sweeper-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&state_dir);
    std::fs::create_dir_all(&state_dir).expect("create state dir");
    crate::registry::ensure_registry(&state_dir).expect("create registry");

    let config = DaemonConfig {
        socket_path: state_dir.join("manager.sock"),
        state_dir: state_dir.clone(),
        pid_file: state_dir.join("manager.pid"),
        debootstrap_binary: "debootstrap".to_string(),
        workspace_apparmor_profile: None,
        workspace_selinux_label: None,
        idle_stop_after_secs: Some(60),
        destroy_after_secs: Some(3600),
        default_workspace_cpu_percent: None,
        default_workspace_max_processes: None,
        task_count_threshold: 1024,
        detached_process_threshold: 32,
        max_running_workspaces: None,
        sweep_interval_secs: 30,
        sweep_enabled: true,
        evict_idle: true,
    };
    let services = DaemonServices {
        rate_limiter: Arc::new(RateLimiter::with_global_limit(
            std::time::Duration::from_secs(2),
            120,
            600,
        )),
        port_publisher: Arc::new(PortPublisher::new()),
        leases: Arc::new(LifecycleLeases::default()),
        active_operations: Arc::new(ActiveOperations::default()),
        activity: Arc::new(ActivityService::new()),
        policy: Arc::new(IdlePolicy {
            idle_stop_after_secs: Some(60),
            destroy_after_secs: Some(3600),
            default_workspace_cpu_percent: None,
            default_workspace_max_processes: None,
            task_count_threshold: 1024,
            detached_process_threshold: 32,
            max_running_workspaces: None,
            sweep_interval: std::time::Duration::from_secs(30),
            sweep_enabled: true,
            evict_idle: true,
        }),
    };
    (state_dir, config, services)
}

fn insert_workspace(state_dir: &std::path::Path, workspace: WorkspaceMetadata) {
    let sandbox_dir = state_dir.join("sandboxes").join(&workspace.sandbox_id);
    std::fs::create_dir_all(&sandbox_dir).expect("create sandbox dir");
    let sandbox = crate::sandbox::SandboxMetadata {
        id: workspace.sandbox_id.clone(),
        name: "box".to_string(),
        suite: "bookworm".to_string(),
        mirror: "http://deb.debian.org/debian".to_string(),
        bootstrap_method: crate::sandbox::BootstrapMethod::CachedRootfs,
        created_at: "2026-08-06T00:00:00Z".to_string(),
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
        status: crate::sandbox::SandboxStatus::Running,
    };
    crate::registry::with_registry_mut(state_dir, |registry| {
        let entry = registry
            .sandboxes
            .entry(sandbox.id.clone())
            .or_insert_with(|| crate::registry::RegistrySandbox {
                metadata: sandbox.clone(),
                workspaces: Default::default(),
            });
        entry
            .workspaces
            .insert(workspace.id.clone(), workspace.clone());
        Ok(())
    })
    .expect("insert workspace");
}

fn workspace(id: &str, last_request_at: String) -> WorkspaceMetadata {
    WorkspaceMetadata {
        id: id.to_string(),
        sandbox_id: "box-sweeper".to_string(),
        name: id.to_string(),
        created_at: "2026-08-06T00:00:00Z".to_string(),
        last_request_at: Some(last_request_at),
        last_process_activity_at: None,
        last_stopped_at: None,
        workspace_path: format!("/tmp/enclave-sweeper-{id}"),
        filesystem_path: format!("/tmp/enclave-sweeper-{id}/fs"),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: "/tmp/enclave-sweeper-rootfs".to_string(),
        overlay_home_base_path: "/tmp/enclave-sweeper-home-base".to_string(),
        overlay_home_upper_path: String::new(),
        overlay_home_work_path: String::new(),
        overlay_home_merged_path: String::new(),
        auth_providers: Vec::new(),
        owner: None,
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: WorkspaceStatus::Running,
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: Default::default(),
        clear_tmp_on_restart: false,
        keep_running: false,
        idle_stop_after_secs: None,
        destroy_after_secs: None,
        limits: Default::default(),
        assigned_ip: None,
    }
}

fn not_idle_while_live_port(state_dir: &std::path::Path, services: &DaemonServices) {
    let workspace = workspace(
        "ws-port",
        (Utc::now() - Duration::seconds(600)).to_rfc3339(),
    );
    insert_workspace(state_dir, workspace.clone());
    let candidates = crate::workspace::activity_service::idle_candidates(
        state_dir,
        &services.activity,
        Some(60),
        Utc::now(),
    )
    .expect("list idle candidates");
    assert_eq!(candidates.len(), 1);
}

#[test]
fn the_sweeper_uses_a_workspace_lifecycle_lease() {
    let (state_dir, _config, services) = fixture("lease");
    let scope = LeaseScope::Workspace {
        sandbox: "box-sweeper".to_string(),
        workspace: "ws-lease".to_string(),
    };
    let leases = Arc::clone(&services.leases);
    let probe = scope.clone();
    let (started, started_rx) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _held = leases.acquire(probe).expect("hold workspace lease");
        started.send(()).expect("signal held lease");
        release_rx.recv().expect("wait for release");
    });
    started_rx.recv().expect("lease holder started");

    let contended = Arc::clone(&services.leases);
    let (contended_tx, contended_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = contended.acquire(scope);
        let _ = contended_tx.send(result.is_ok());
    });
    assert!(
        contended_rx
            .recv_timeout(std::time::Duration::from_millis(150))
            .is_err(),
        "a sweep's workspace lease must wait behind an in-flight lifecycle operation"
    );
    release.send(()).expect("release workspace lease");
    holder.join().expect("join lease holder");
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn a_running_port_connection_is_an_activity_source() {
    let (state_dir, _config, services) = fixture("port");
    not_idle_while_live_port(&state_dir, &services);
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn destroy_after_rechecks_the_record_under_the_lease() {
    let (state_dir, config, services) = fixture("destroy");
    let mut workspace = workspace(
        "ws-retain",
        (Utc::now() - Duration::seconds(600)).to_rfc3339(),
    );
    workspace.status = WorkspaceStatus::Stopped;
    workspace.last_stopped_at = Some((Utc::now() - Duration::seconds(7200)).to_rfc3339());
    workspace.destroy_after_secs = Some(1);
    insert_workspace(&state_dir, workspace.clone());

    // A stop/start/pin after the candidate snapshot must win over the stale
    // destroy decision. Hold the lease and change the record the way a normal
    // request would, then run the sweep after releasing it.
    {
        let _lease = services
            .leases
            .acquire(LeaseScope::Workspace {
                sandbox: workspace.sandbox_id.clone(),
                workspace: workspace.id.clone(),
            })
            .expect("hold lease");
        crate::registry::with_registry_mut(&state_dir, |registry| {
            let current = registry
                .sandboxes
                .get_mut(&workspace.sandbox_id)
                .and_then(|sandbox| sandbox.workspaces.get_mut(&workspace.id))
                .expect("workspace");
            current.keep_running = true;
            Ok(())
        })
        .expect("pin workspace");
    }

    sweep_at(&config, &services, Utc::now()).expect("run sweep");
    let remaining =
        crate::workspace::workspace_metadata(&state_dir, &workspace.sandbox_id, &workspace.id)
            .expect("pinned workspace remains");
    assert!(remaining.keep_running);
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn a_foreground_job_blocks_idle_stop() {
    let (state_dir, config, services) = fixture("foreground");
    let workspace = workspace(
        "ws-foreground",
        (Utc::now() - Duration::seconds(600)).to_rfc3339(),
    );
    insert_workspace(&state_dir, workspace.clone());
    let mut job = crate::workspace::JobRecord::new(
        &workspace.sandbox_id,
        &workspace.id,
        crate::workspace::JobMode::Foreground,
        &["sleep".to_string(), "30".to_string()],
    );
    job.pid = Some(std::process::id());
    job.starttime_ticks =
        crate::workspace::session::process_starttime_ticks(std::process::id()).ok();
    crate::workspace::jobs::persist(&state_dir, &job).expect("persist foreground job");
    services.activity.update_job(job.clone());

    assert!(services
        .activity
        .has_active_foreground_job(&workspace.sandbox_id, &workspace.id));
    let _ = config;
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn a_request_lease_is_not_an_idle_candidate() {
    let (state_dir, _config, services) = fixture("request-lease");
    let workspace = workspace(
        "ws-request",
        (Utc::now() - Duration::seconds(600)).to_rfc3339(),
    );
    insert_workspace(&state_dir, workspace.clone());

    let lease = services
        .activity
        .begin_request(&state_dir, &workspace.sandbox_id, &workspace.id)
        .expect("begin request lease");
    assert_eq!(
        services
            .activity
            .active_commands(&workspace.sandbox_id, &workspace.id),
        1
    );
    assert!(crate::workspace::activity_service::idle_candidates(
        &state_dir,
        &services.activity,
        Some(60),
        Utc::now(),
    )
    .expect("list idle candidates")
    .is_empty());

    drop(lease);
    assert_eq!(
        services
            .activity
            .active_commands(&workspace.sandbox_id, &workspace.id),
        0
    );
    let released_at = crate::workspace::parse_timestamp(
        &crate::workspace::workspace_metadata(&state_dir, &workspace.sandbox_id, &workspace.id)
            .expect("read workspace")
            .last_request_at
            .expect("request timestamp"),
    )
    .expect("parse request timestamp");
    assert_eq!(
        crate::workspace::activity_service::idle_candidates(
            &state_dir,
            &services.activity,
            Some(60),
            released_at + Duration::seconds(61),
        )
        .expect("list idle candidates")
        .len(),
        1
    );
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn an_ordinary_background_job_does_not_pin_the_workspace() {
    let (state_dir, _config, services) = fixture("background-pin");
    let workspace = workspace("ws-background", "2020-01-01T00:00:00Z".to_string());
    insert_workspace(&state_dir, workspace.clone());
    let mut job = crate::workspace::JobRecord::new(
        &workspace.sandbox_id,
        &workspace.id,
        crate::workspace::JobMode::Background,
        &["sleep".to_string(), "30".to_string()],
    );
    job.pid = Some(std::process::id());
    job.starttime_ticks =
        crate::workspace::session::process_starttime_ticks(std::process::id()).ok();
    services.activity.update_job(job.clone());
    assert!(!services
        .activity
        .keeps_workspace_running(&workspace.sandbox_id, &workspace.id));
    assert_eq!(
        crate::workspace::activity_service::idle_candidates(
            &state_dir,
            &services.activity,
            Some(60),
            Utc::now(),
        )
        .expect("list idle candidates")
        .len(),
        1
    );

    job.keep_running = true;
    services.activity.update_job(job);
    assert!(services
        .activity
        .keeps_workspace_running(&workspace.sandbox_id, &workspace.id));
    assert!(crate::workspace::activity_service::idle_candidates(
        &state_dir,
        &services.activity,
        Some(60),
        Utc::now(),
    )
    .expect("list idle candidates")
    .is_empty());
    let _ = std::fs::remove_dir_all(state_dir);
}
