use enclave::sandbox::{BootstrapMethod, RootfsTier, SandboxLimits, SandboxMetadata};

#[test]
fn bootstrap_method_default_is_debootstrap() {
    assert_eq!(BootstrapMethod::default(), BootstrapMethod::Debootstrap);
}

#[test]
fn bootstrap_method_display() {
    assert_eq!(BootstrapMethod::Debootstrap.to_string(), "debootstrap");
    assert_eq!(BootstrapMethod::CachedRootfs.to_string(), "cached_rootfs");
}

#[test]
fn bootstrap_method_from_str_valid() {
    assert_eq!(
        "debootstrap".parse::<BootstrapMethod>().unwrap(),
        BootstrapMethod::Debootstrap
    );
    assert_eq!(
        "cached_rootfs".parse::<BootstrapMethod>().unwrap(),
        BootstrapMethod::CachedRootfs
    );
}

#[test]
fn bootstrap_method_from_str_invalid() {
    let result = "docker".parse::<BootstrapMethod>();
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("unknown bootstrap method"), "got: {}", msg);
}

#[test]
fn bootstrap_method_serde_roundtrip() {
    let json = serde_json::to_string(&BootstrapMethod::CachedRootfs).unwrap();
    assert_eq!(json, "\"cached_rootfs\"");
    let parsed: BootstrapMethod = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, BootstrapMethod::CachedRootfs);
}

#[test]
fn bootstrap_method_deserializes_default() {
    let json = r#"{
        "id": "test-123",
        "name": "test",
        "suite": "bookworm",
        "mirror": "http://deb.debian.org/debian",
        "created_at": "2024-01-01T00:00:00Z",
        "sandbox_path": "/tmp/test",
        "rootfs_path": "/tmp/test/rootfs"
    }"#;
    let metadata: SandboxMetadata = serde_json::from_str(json).unwrap();
    assert_eq!(metadata.bootstrap_method, BootstrapMethod::Debootstrap);
    assert_eq!(metadata.limits, SandboxLimits::default());
}

/// The base-image tier is part of the control protocol, so its wire value is a
/// contract rather than a display string.
///
/// A caller that wants to know whether a sandbox is sharing the cached rootfs or
/// paying for its own copy branches on this value, so renaming it silently is the
/// same kind of break as renaming a field. The labels are pinned here for that
/// reason, and the display sentence that goes with each one lives in the command
/// output where it can be reworded freely.
#[test]
fn rootfs_tier_wire_values_are_stable() {
    let shared = serde_json::to_string(&RootfsTier::SharedOverlay).unwrap();
    assert_eq!(shared, "\"shared_overlay\"");
    let copied = serde_json::to_string(&RootfsTier::Copied).unwrap();
    assert_eq!(copied, "\"copied\"");

    let parsed: RootfsTier = serde_json::from_str(&shared).unwrap();
    assert_eq!(parsed, RootfsTier::SharedOverlay);
    assert_eq!(RootfsTier::SharedOverlay.as_str(), "shared_overlay");
    assert_eq!(RootfsTier::Copied.as_str(), "copied");
}
