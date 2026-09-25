use super::*;

use crate::fsutil::{remove_creation_marker, write_creation_marker};

pub fn create_sandbox(
    state_dir: &Path,
    debootstrap_binary: &str,
    name: &str,
    suite: &str,
    mirror: &str,
    method: &BootstrapMethod,
) -> Result<SandboxMetadata> {
    create_sandbox_with_options(
        state_dir,
        debootstrap_binary,
        name,
        suite,
        mirror,
        method,
        SandboxCreateOptions::default(),
    )
}

#[derive(Debug, Clone, Default)]

pub struct SandboxCreateOptions {
    pub limits: SandboxLimits,
}

pub fn create_sandbox_with_options(
    state_dir: &Path,
    debootstrap_binary: &str,
    name: &str,
    suite: &str,
    mirror: &str,
    method: &BootstrapMethod,
    options: SandboxCreateOptions,
) -> Result<SandboxMetadata> {
    validate_name(name)?;
    options.limits.validate()?;

    if *method == BootstrapMethod::Debootstrap {
        validate_debootstrap_inputs(suite, mirror)?;
    }

    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        bail!(
            "sandbox creation requires root privileges (debootstrap must run as root). \
             Re-run with sudo."
        );
    }

    init_storage(state_dir)?;

    let sandbox_id = generate_sandbox_id(name);
    let sandbox_dir = sandboxes_dir(state_dir).join(&sandbox_id);
    let rootfs_dir = sandbox_dir.join("rootfs");
    let runtime_dir = sandbox_dir.join("runtime");
    let mounted_rootfs_dir = runtime_dir.join("rootfs.mnt");
    let workspaces_dir = sandbox_dir.join("workspaces");
    let home_base_dir = sandbox_dir.join("home-base");

    let create_result = (|| {
        fs::create_dir(&sandbox_dir).with_context(|| {
            format!(
                "failed to create sandbox directory {}",
                sandbox_dir.to_string_lossy()
            )
        })?;
        // Claim the directory before anything is written into it. The registry
        // record only appears at the end, after the bootstrap, so without this a
        // concurrent repair would see a directory with no `sandbox.json` and
        // remove it as an orphan while this create is still filling it.
        write_creation_marker(&sandbox_dir)?;
        fs::create_dir(&rootfs_dir).with_context(|| {
            format!(
                "failed to create rootfs directory {}",
                rootfs_dir.to_string_lossy()
            )
        })?;
        fs::create_dir(&runtime_dir).with_context(|| {
            format!(
                "failed to create runtime directory {}",
                runtime_dir.to_string_lossy()
            )
        })?;
        fs::create_dir(&mounted_rootfs_dir).with_context(|| {
            format!(
                "failed to create mounted rootfs directory {}",
                mounted_rootfs_dir.to_string_lossy()
            )
        })?;
        fs::create_dir(&workspaces_dir).with_context(|| {
            format!(
                "failed to create workspaces directory {}",
                workspaces_dir.to_string_lossy()
            )
        })?;
        fs::create_dir(&home_base_dir).with_context(|| {
            format!(
                "failed to create home base directory {}",
                home_base_dir.to_string_lossy()
            )
        })?;
        Ok::<(), anyhow::Error>(())
    })();
    if let Err(err) = create_result {
        if sandbox_dir.exists() {
            fs::remove_dir_all(&sandbox_dir).with_context(|| {
                format!(
                    "failed to clean up partial sandbox directory {}",
                    sandbox_dir.display()
                )
            })?;
        }
        return Err(err);
    }

    let bootstrap_result = bootstrap::bootstrap_rootfs(&bootstrap::BootstrapParams {
        method,
        rootfs_dir: &rootfs_dir,
        sandbox_dir: &sandbox_dir,
        debootstrap_binary,
        suite,
        mirror,
        name,
        state_dir,
    });
    let outcome = match bootstrap_result {
        Ok(outcome) => outcome,
        Err(err) => {
            if sandbox_dir.exists() {
                fs::remove_dir_all(&sandbox_dir).with_context(|| {
                    format!(
                        "failed to clean up sandbox directory after bootstrap failure {}",
                        sandbox_dir.display()
                    )
                })?;
            }
            return Err(err);
        }
    };

    // A shared base is mounted as an overlay so the cached rootfs is never
    // copied. If the mount cannot be set up, fall back to the copy so sandbox
    // creation still succeeds on kernels or filesystems without OverlayFS.
    let shared_lower = match outcome.shared_lower {
        Some(lower) => {
            let shared = SandboxMetadata {
                id: sandbox_id.clone(),
                name: name.to_string(),
                sandbox_path: sandbox_dir.to_string_lossy().to_string(),
                rootfs_path: rootfs_dir.to_string_lossy().to_string(),
                rootfs_lower_path: Some(lower.to_string_lossy().to_string()),
                mounted_rootfs_path: mounted_rootfs_dir.to_string_lossy().to_string(),
                ..SandboxMetadata::default()
            };
            match mounts::ensure_rootfs_overlay_mounted(&shared) {
                Ok(()) => Some(lower.to_string_lossy().to_string()),
                Err(err) => {
                    tracing::warn!(
                        "sandbox '{}': shared rootfs base unavailable ({err:#}); copying instead",
                        name
                    );
                    let _ = mounts::unmount_rootfs_overlay(&shared);
                    if let Err(copy_err) = bootstrap::copy_cached_rootfs(&lower, &rootfs_dir) {
                        let _ = fs::remove_dir_all(&sandbox_dir);
                        return Err(copy_err);
                    }
                    None
                }
            }
        }
        None => None,
    };

    let metadata = SandboxMetadata {
        id: sandbox_id,
        name: name.to_string(),
        suite: suite.to_string(),
        mirror: mirror.to_string(),
        bootstrap_method: method.clone(),
        created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        sandbox_path: sandbox_dir.to_string_lossy().to_string(),
        rootfs_path: rootfs_dir.to_string_lossy().to_string(),
        rootfs_lower_path: shared_lower,
        mounted_rootfs_path: mounted_rootfs_dir.to_string_lossy().to_string(),
        workspaces_path: workspaces_dir.to_string_lossy().to_string(),
        home_base_path: home_base_dir.to_string_lossy().to_string(),
        limits: options.limits,
        status: SandboxStatus::Stopped,
    };

    let metadata_path = sandbox_dir.join("sandbox.json");
    let metadata_json = serde_json::to_string_pretty(&metadata)?;
    crate::fsutil::write_file_atomic(&metadata_path, metadata_json.as_bytes(), 0o600)
        .with_context(|| {
            format!(
                "failed to write sandbox metadata at {}",
                metadata_path.to_string_lossy()
            )
        })?;

    with_registry_mut(state_dir, |registry| {
        registry.sandboxes.insert(
            metadata.id.clone(),
            RegistrySandbox {
                metadata: metadata.clone(),
                workspaces: Default::default(),
            },
        );
        Ok(())
    })?;

    remove_creation_marker(&sandbox_dir);

    Ok(metadata)
}
