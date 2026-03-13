use super::*;

#[test]
fn cgroup_config_from_limits_no_limits() {
    let config = CgroupConfig::from_limits(None, None, None).unwrap();
    assert!(!config.has_limits());
}

#[test]
fn cgroup_config_from_limits_all_limits() {
    let config = CgroupConfig::from_limits(Some(1024 * 1024), Some(25.0), Some(100)).unwrap();
    assert!(config.has_limits());
    assert_eq!(config.memory_bytes, Some(1024 * 1024));
    assert_eq!(
        config.cpu_quota_us,
        Some(
            crate::resource_limits::cpu_quota_from_machine_percent(25.0, config.cpu_period_us)
                .unwrap()
        )
    );
    assert_eq!(config.cpu_period_us, 100_000);
    assert_eq!(config.pids_max, Some(100));
}

#[test]
fn cgroup_config_partial_limits() {
    let config = CgroupConfig::from_limits(Some(512 * 1024), None, None).unwrap();
    assert!(config.has_limits());
    assert_eq!(config.memory_bytes, Some(512 * 1024));
    assert!(config.cpu_quota_us.is_none());
    assert!(config.pids_max.is_none());
}

#[test]
fn cgroup_v2_availability_check() {
    let _available = is_cgroup_v2_available();
}

#[test]
fn available_controllers_does_not_panic() {
    let _controllers = available_controllers();
}

#[test]
fn cgroup_stats_display() {
    let stats = CgroupStats {
        memory_current_bytes: Some(4096),
        memory_max_bytes: Some("1048576".to_string()),
        pids_current: Some(5),
        pids_max: Some("100".to_string()),
        cpu_max: Some("50000 100000".to_string()),
    };
    let display = stats.to_string();
    assert!(display.contains("memory=4096B/1048576"), "got: {}", display);
    assert!(display.contains("pids=5/100"), "got: {}", display);
    assert!(display.contains("cpu_max=50000 100000"), "got: {}", display);
}

#[test]
fn cgroup_stats_display_defaults() {
    let stats = CgroupStats::default();
    let display = stats.to_string();
    assert!(display.contains("memory=unknown/max"), "got: {}", display);
    assert!(display.contains("pids=unknown/max"), "got: {}", display);
}

#[test]
fn remove_nonexistent_cgroup_is_ok() {
    let result = remove_workspace_cgroup("enclave-nonexistent-test-cgroup-12345");
    assert!(result.is_ok());
}

#[test]
fn create_workspace_cgroup_returns_none_when_no_limits() {
    let config = CgroupConfig::default();
    let result = create_workspace_cgroup("test-ws", &config).unwrap();
    assert!(result.is_none());
}

#[test]
fn validate_cgroup_name_rejects_traversal() {
    assert!(validate_cgroup_name("..").is_err());
    assert!(validate_cgroup_name("/sys/fs/cgroup").is_err());
    assert!(validate_cgroup_name("foo/bar").is_err());
    assert!(validate_cgroup_name("foo\\bar").is_err());
    assert!(validate_cgroup_name("").is_err());
}

#[test]
fn validate_cgroup_name_accepts_safe_names() {
    assert!(validate_cgroup_name("enclave-ws-12345").is_ok());
    assert!(validate_cgroup_name("my_workspace.1").is_ok());
}

#[test]
fn validate_cgroup_name_rejects_single_dot() {
    assert!(validate_cgroup_name(".").is_err());
}

#[test]
fn validate_cgroup_name_rejects_spaces_and_special() {
    assert!(validate_cgroup_name("name with spaces").is_err());
    assert!(validate_cgroup_name("name@host").is_err());
    assert!(validate_cgroup_name("name!").is_err());
    assert!(validate_cgroup_name("name#tag").is_err());
}

#[test]
fn cgroup_config_cpu_only() {
    let config = CgroupConfig::from_limits(None, Some(30.0), None).unwrap();
    assert!(config.has_limits());
    assert!(config.memory_bytes.is_none());
    assert_eq!(
        config.cpu_quota_us,
        Some(
            crate::resource_limits::cpu_quota_from_machine_percent(30.0, config.cpu_period_us)
                .unwrap()
        )
    );
    assert!(config.pids_max.is_none());
}

#[test]
fn cgroup_config_pids_only() {
    let config = CgroupConfig::from_limits(None, None, Some(50)).unwrap();
    assert!(config.has_limits());
    assert!(config.memory_bytes.is_none());
    assert!(config.cpu_quota_us.is_none());
    assert_eq!(config.pids_max, Some(50));
}

#[test]
fn cgroup_config_rejects_zero_cpu_percent() {
    let result = CgroupConfig::from_limits(None, Some(0.0), None);
    assert!(result.is_err());
}

#[test]
fn cgroup_config_large_memory_limit() {
    let gb = 1024 * 1024 * 1024;
    let config = CgroupConfig::from_limits(Some(8 * gb), None, None).unwrap();
    assert!(config.has_limits());
    assert_eq!(config.memory_bytes, Some(8 * gb));
}

#[test]
fn cgroup_stats_partial_values() {
    let stats = CgroupStats {
        memory_current_bytes: Some(0),
        memory_max_bytes: None,
        pids_current: None,
        pids_max: Some("max".to_string()),
        cpu_max: None,
    };
    let display = stats.to_string();
    assert!(display.contains("memory=0B/max"), "got: {}", display);
    assert!(display.contains("pids=unknown/max"), "got: {}", display);
    assert!(display.contains("cpu_max=max"), "got: {}", display);
}

#[test]
fn remove_cgroup_validates_name() {
    let result = remove_workspace_cgroup("..");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("unsafe"), "got: {}", msg);
}

#[test]
fn create_cgroup_validates_name() {
    let config = CgroupConfig::from_limits(Some(1024), None, None).unwrap();
    let result = create_workspace_cgroup("../escape", &config);
    assert!(result.is_err());
}
