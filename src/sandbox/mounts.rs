use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use super::types::SandboxMetadata;

pub fn ensure_rootfs_mounted(metadata: &SandboxMetadata) -> Result<()> {
    ensure_rootfs_overlay_mounted(metadata)?;
    let (rootfs_path, mounted_rootfs_path) = validate_mount_paths(metadata)?;
    if crate::fsutil::is_mountpoint(Path::new(&mounted_rootfs_path))? {
        return Ok(());
    }

    crate::fsutil::bind_mount(Path::new(&rootfs_path), Path::new(&mounted_rootfs_path))
        .map_err(|error| anyhow!("failed to mount sandbox rootfs: {error}"))?;
    crate::fsutil::make_mount_private(Path::new(&mounted_rootfs_path))
        .map_err(|error| anyhow!("failed to mark mount private: {error}"))?;

    Ok(())
}

/// Directory names for the writable layers of a shared-base sandbox rootfs.
const ROOTFS_UPPER_DIR: &str = "rootfs-upper";
const ROOTFS_WORK_DIR: &str = "rootfs-work";

/// Paths of the upper and work directories backing a shared-base rootfs.
pub fn rootfs_overlay_paths(metadata: &SandboxMetadata) -> Option<(PathBuf, PathBuf)> {
    metadata.rootfs_lower_path.as_ref()?;
    let sandbox_path = Path::new(&metadata.sandbox_path);
    Some((
        sandbox_path.join(ROOTFS_UPPER_DIR),
        sandbox_path.join(ROOTFS_WORK_DIR),
    ))
}

/// Mount the sandbox rootfs as an OverlayFS whose lower layer is the shared
/// cached base.
///
/// Copying a cached rootfs is metadata bound: a Debian rootfs is tens of
/// thousands of small files, and the copy costs seconds per sandbox. Mounting
/// an overlay over the shared base gives each sandbox its own writable view in
/// constant time, and writes never reach the shared base because it is only
/// ever a lower layer.
///
/// Sandboxes without a shared base keep their plain rootfs directory.
pub fn ensure_rootfs_overlay_mounted(metadata: &SandboxMetadata) -> Result<()> {
    let Some(lower) = metadata.rootfs_lower_path.as_deref() else {
        return Ok(());
    };
    let lower = Path::new(lower);
    if !lower.is_dir() {
        bail!(
            "sandbox '{}' shared rootfs base {} is missing",
            metadata.id,
            lower.display()
        );
    }
    let rootfs_path = Path::new(&metadata.rootfs_path);
    if crate::fsutil::is_mountpoint(rootfs_path)? {
        return Ok(());
    }
    let Some((upper, work)) = rootfs_overlay_paths(metadata) else {
        return Ok(());
    };
    for path in [&upper, &work, rootfs_path] {
        fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    }
    let options = format!(
        "lowerdir={},upperdir={},workdir={}",
        overlay_option_path(lower),
        overlay_option_path(&upper),
        overlay_option_path(&work)
    );
    mount_overlay(rootfs_path, &options).with_context(|| {
        format!(
            "failed to mount shared rootfs base {} for sandbox '{}'",
            lower.display(),
            metadata.id
        )
    })?;
    if !crate::fsutil::is_mountpoint(rootfs_path)? {
        bail!(
            "sandbox rootfs overlay {} is not mounted after mount_overlay",
            rootfs_path.display()
        );
    }
    Ok(())
}

/// Unmount the sandbox rootfs overlay. Sandboxes without a shared base are
/// untouched, so their plain rootfs directory survives.
pub fn unmount_rootfs_overlay(metadata: &SandboxMetadata) -> Result<()> {
    if metadata.rootfs_lower_path.is_none() {
        return Ok(());
    }
    let rootfs_path = Path::new(&metadata.rootfs_path);
    if !crate::fsutil::is_mountpoint(rootfs_path)? {
        return Ok(());
    }
    if let Err(error) = unmount_path(rootfs_path) {
        if is_already_unmounted_errno(error.raw_os_error()) {
            return Ok(());
        }
        return Err(anyhow!(
            "failed to unmount sandbox rootfs overlay mount_target={} errno={} namespace_holders={} detail={}",
            rootfs_path.display(),
            format_errno(error.raw_os_error()),
            mount_holders(&metadata.rootfs_path),
            error
        ));
    }
    Ok(())
}

fn mount_overlay(target: &Path, options: &str) -> std::io::Result<()> {
    let source = CString::new("overlay").expect("literal contains no nul");
    let filesystem = CString::new("overlay").expect("literal contains no nul");
    let target = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let options =
        CString::new(options).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let result = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            filesystem.as_ptr(),
            0,
            options.as_ptr().cast(),
        )
    };
    if result == 0 {
        crate::perf::record_mount();
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Escape the characters OverlayFS uses as option separators.
fn overlay_option_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\134")
        .replace(',', "\\054")
        .replace(':', "\\072")
}

/// Make sure a running sandbox hands its workspaces a usable rootfs.
///
/// The rootfs a workspace session receives is the bind mount the daemon owns at
/// `mounted_rootfs_path`, not the rootfs directory itself. That mount is host
/// mount state, so a stop/start cycle or any interrupted teardown can leave the
/// registry saying `running` while the bind is gone. A workspace started in that
/// state would see an empty root and still be recorded as running, which is a
/// silent failure rather than a reported one.
///
/// Re-establish the mount, which is idempotent, and then prove the result is a
/// mount point holding a populated rootfs.
pub fn ensure_rootfs_ready_for_workspace(metadata: &SandboxMetadata) -> Result<()> {
    ensure_rootfs_mounted(metadata)?;
    let mounted_rootfs_path = Path::new(&metadata.mounted_rootfs_path);
    if !crate::fsutil::is_mountpoint(mounted_rootfs_path)? {
        bail!(
            "sandbox '{}' rootfs {} is not mounted; the sandbox cannot hand a root filesystem to a workspace",
            metadata.id,
            mounted_rootfs_path.display()
        );
    }
    let mut entries = fs::read_dir(mounted_rootfs_path).with_context(|| {
        format!(
            "failed to read sandbox '{}' rootfs {}",
            metadata.id,
            mounted_rootfs_path.display()
        )
    })?;
    if entries.next().is_none() {
        bail!(
            "sandbox '{}' rootfs {} is empty; the sandbox was not bootstrapped or its rootfs was replaced",
            metadata.id,
            mounted_rootfs_path.display()
        );
    }
    Ok(())
}

pub fn ensure_rootfs_unmounted(metadata: &SandboxMetadata) -> Result<()> {
    let Some(mounted_rootfs_path) = validate_unmount_path(metadata)? else {
        return Ok(());
    };
    if !crate::fsutil::is_mountpoint(Path::new(&mounted_rootfs_path))? {
        return Ok(());
    }

    if let Err(error) = unmount_path(Path::new(&mounted_rootfs_path)) {
        if is_already_unmounted_errno(error.raw_os_error()) {
            return Ok(());
        }
        return Err(anyhow!(
            "failed to unmount sandbox rootfs mount_target={} errno={} namespace_holders={} detail={}",
            mounted_rootfs_path,
            format_errno(error.raw_os_error()),
            mount_holders(&mounted_rootfs_path),
            error
        ));
    }
    if crate::fsutil::is_mountpoint(Path::new(&mounted_rootfs_path))? {
        bail!(
            "sandbox rootfs mount {} is still present after umount",
            mounted_rootfs_path
        );
    }
    Ok(())
}

fn unmount_path(path: &Path) -> std::io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let result = unsafe { libc::umount2(path.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn is_already_unmounted_errno(errno: Option<i32>) -> bool {
    matches!(errno, Some(libc::ENOENT | libc::EINVAL))
}

fn format_errno(errno: Option<i32>) -> String {
    match errno {
        Some(libc::EBUSY) => "EBUSY(16)".to_string(),
        Some(libc::ENOENT) => "ENOENT(2)".to_string(),
        Some(libc::EINVAL) => "EINVAL(22)".to_string(),
        Some(libc::EPERM) => "EPERM(1)".to_string(),
        Some(value) => format!("errno({value})"),
        None => "unknown".to_string(),
    }
}

fn mount_holders(path: &str) -> String {
    let Ok(entries) = fs::read_dir("/proc") else {
        return "none".to_string();
    };
    let mut holders = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|value| value.chars().all(|ch| ch.is_ascii_digit()))
        else {
            continue;
        };
        let Ok(mountinfo) = fs::read_to_string(entry.path().join("mountinfo")) else {
            continue;
        };
        let owns_mount = mountinfo
            .lines()
            .any(|line| line.split_whitespace().nth(4) == Some(path));
        if !owns_mount {
            continue;
        }
        let namespace = fs::read_link(entry.path().join("ns/mnt"))
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        holders.push(format!("pid={pid}@{namespace}"));
        if holders.len() == 8 {
            break;
        }
    }
    if holders.is_empty() {
        "none".to_string()
    } else {
        holders.join(",")
    }
}

fn validate_unmount_path(metadata: &SandboxMetadata) -> Result<Option<String>> {
    let sandbox_dir = PathBuf::from(&metadata.sandbox_path);
    let sandbox_metadata = match fs::symlink_metadata(&sandbox_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to stat sandbox path {}", sandbox_dir.display()))
        }
    };
    if sandbox_metadata.file_type().is_symlink() {
        bail!(
            "sandbox path {} must not be a symlink",
            sandbox_dir.display()
        );
    }

    let mounted_rootfs_path = PathBuf::from(&metadata.mounted_rootfs_path);
    match fs::symlink_metadata(&mounted_rootfs_path) {
        Ok(path_metadata) if path_metadata.file_type().is_symlink() => {
            bail!(
                "mounted rootfs path {} must not be a symlink",
                mounted_rootfs_path.display()
            )
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to stat mounted rootfs path {}",
                    mounted_rootfs_path.display()
                )
            })
        }
    }
    let canonical_mounted = crate::fsutil::ensure_path_within(
        &sandbox_dir,
        &mounted_rootfs_path,
        "mounted rootfs path",
    )?;

    let rootfs_path = PathBuf::from(&metadata.rootfs_path);
    reject_symlink_if_present("rootfs path", &rootfs_path)?;
    if rootfs_path.exists() {
        let canonical_rootfs =
            crate::fsutil::ensure_path_within(&sandbox_dir, &rootfs_path, "rootfs path")?;
        if canonical_rootfs == canonical_mounted {
            bail!("rootfs and mounted rootfs paths must not be the same");
        }
    }

    Ok(Some(canonical_mounted.to_string_lossy().to_string()))
}

fn reject_symlink_if_present(label: &str, path: &std::path::Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("{} {} must not be a symlink", label, path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to stat {} {}", label, path.display()))
        }
    }
}

fn validate_mount_paths(metadata: &SandboxMetadata) -> Result<(String, String)> {
    let sandbox_dir = PathBuf::from(&metadata.sandbox_path);
    let rootfs_path = PathBuf::from(&metadata.rootfs_path);
    let mounted_rootfs_path = PathBuf::from(&metadata.mounted_rootfs_path);

    for (label, path) in [
        ("sandbox path", &sandbox_dir),
        ("rootfs path", &rootfs_path),
        ("mounted rootfs path", &mounted_rootfs_path),
    ] {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("failed to stat {} {}", label, path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("{} {} must not be a symlink", label, path.display());
        }
    }

    let canonical_rootfs =
        crate::fsutil::canonicalize_within(&sandbox_dir, &rootfs_path, "rootfs path")?;
    let canonical_mounted = crate::fsutil::canonicalize_within(
        &sandbox_dir,
        &mounted_rootfs_path,
        "mounted rootfs path",
    )?;
    if canonical_rootfs == canonical_mounted {
        bail!("rootfs and mounted rootfs paths must not be the same");
    }

    Ok((
        canonical_rootfs.to_string_lossy().to_string(),
        canonical_mounted.to_string_lossy().to_string(),
    ))
}

#[cfg(test)]
#[path = "../../tests/src/sandbox/mounts.rs"]
mod tests;
