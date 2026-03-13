use enclave::sandbox::{BootstrapMethod, SandboxLimits, SandboxMetadata};

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
