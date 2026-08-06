use super::*;
use std::collections::BTreeMap;
use std::fs;

use crate::registry::{with_registry, with_registry_mut, RegistrySandbox};
use crate::sandbox::{BootstrapMethod, SandboxLimits, SandboxStatus};

#[test]
fn destroy_removes_registry_entry_before_failed_unmount_cleanup() {
    let temp_dir =
        std::env::temp_dir().join(format!("enclave-destroy-registry-{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox_dir = temp_dir.join("sandboxes").join("sandbox-id");
    fs::create_dir_all(sandbox_dir.join("runtime")).unwrap();
    fs::create_dir_all(&temp_dir).unwrap();

    let metadata = SandboxMetadata {
        id: "sandbox-id".to_string(),
        name: "sandbox".to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: BootstrapMethod::CachedRootfs,
        created_at: "2026-08-06T00:00:00Z".to_string(),
        sandbox_path: sandbox_dir.to_string_lossy().to_string(),
        rootfs_path: sandbox_dir.join("rootfs").to_string_lossy().to_string(),
        mounted_rootfs_path: sandbox_dir
            .join("runtime")
            .join("rootfs.mnt")
            .to_string_lossy()
            .to_string(),
        workspaces_path: sandbox_dir.join("workspaces").to_string_lossy().to_string(),
        home_base_path: sandbox_dir.join("home-base").to_string_lossy().to_string(),
        limits: SandboxLimits::default(),
        status: SandboxStatus::Stopped,
    };
    std::os::unix::fs::symlink("/tmp", &metadata.mounted_rootfs_path).unwrap();

    with_registry_mut(&temp_dir, |registry| {
        registry.sandboxes.insert(
            metadata.id.clone(),
            RegistrySandbox {
                metadata: metadata.clone(),
                workspaces: BTreeMap::new(),
            },
        );
        Ok(())
    })
    .unwrap();

    let error = destroy_sandbox(&temp_dir, "sandbox").unwrap_err();
    assert!(format!("{error:#}").contains("must not be a symlink"));

    with_registry(&temp_dir, |registry| {
        assert!(!registry.sandboxes.contains_key("sandbox-id"));
        Ok(())
    })
    .unwrap();

    let _ = fs::remove_dir_all(&temp_dir);
}
