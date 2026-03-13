use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

pub fn check_overlayfs() -> Result<()> {
    let filesystems = fs::read_to_string("/proc/filesystems").with_context(|| {
        "failed to read /proc/filesystems; ensure /proc is mounted and accessible"
    })?;
    if filesystems.lines().any(|line| line.contains("overlay")) {
        return Ok(());
    }
    bail!(
        "OverlayFS is not available on this kernel. \
         Ensure the 'overlay' module is loaded (modprobe overlay) \
         or that your kernel was built with CONFIG_OVERLAY_FS."
    )
}

pub fn check_mount_namespace() -> Result<()> {
    let ns_path = Path::new("/proc/self/ns/mnt");
    if ns_path.exists() {
        return Ok(());
    }
    bail!(
        "mount namespace support is not available (/proc/self/ns/mnt missing). \
         Enclave requires a kernel with CONFIG_NAMESPACES enabled."
    )
}

pub fn check_pid_namespace() -> Result<()> {
    let ns_path = Path::new("/proc/self/ns/pid");
    if ns_path.exists() {
        return Ok(());
    }
    bail!(
        "PID namespace support is not available (/proc/self/ns/pid missing). \
         Enclave requires a kernel with CONFIG_PID_NS enabled."
    )
}

pub fn check_net_namespace() -> Result<()> {
    let ns_path = Path::new("/proc/self/ns/net");
    if ns_path.exists() {
        return Ok(());
    }
    bail!(
        "network namespace support is not available (/proc/self/ns/net missing). \
         Enclave requires a kernel with CONFIG_NET_NS enabled."
    )
}

pub fn check_user_namespace() -> Result<()> {
    let ns_path = Path::new("/proc/self/ns/user");
    if ns_path.exists() {
        return Ok(());
    }
    bail!(
        "user namespace support is not available (/proc/self/ns/user missing). \
         Enclave hardened workspaces require CONFIG_USER_NS."
    )
}

pub fn check_cgroup_v2() -> bool {
    super::cgroup::is_cgroup_v2_available()
}

pub fn validate_platform() -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    if let Err(e) = check_overlayfs() {
        errors.push(e.to_string());
    }
    if let Err(e) = check_mount_namespace() {
        errors.push(e.to_string());
    }
    if let Err(e) = check_pid_namespace() {
        errors.push(e.to_string());
    }
    if let Err(e) = check_net_namespace() {
        errors.push(e.to_string());
    }
    if let Err(e) = check_user_namespace() {
        errors.push(e.to_string());
    }

    if errors.is_empty() {
        if !check_cgroup_v2() {
            tracing::info!(
                "cgroup v2 unified hierarchy not available; \
                 resource limits will use rlimits only"
            );
        }
        Ok(())
    } else {
        bail!(
            "platform feature checks failed:\n  - {}",
            errors.join("\n  - ")
        )
    }
}

#[cfg(test)]
#[path = "../../tests/src/sandbox/features.rs"]
mod tests;
