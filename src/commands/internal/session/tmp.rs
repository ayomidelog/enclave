use super::*;

pub(crate) fn mount_workspace_tmp_if_needed(
    old_root: &Path,
    workspace_fs: &Path,
    target: &Path,
    workspace_idmap_option: &str,
    disk_backed_tmp: bool,
) -> Result<()> {
    fs::create_dir_all(target).with_context(|| format!("failed to create {}", target.display()))?;
    if disk_backed_tmp {
        let workspace_tmp = workspace_fs.join(crate::workspace::WORKSPACE_TMP_DIR);
        ensure_workspace_tmp_source(old_root, &workspace_tmp)?;
        let source = path_inside_old_root(old_root, &workspace_tmp)?;
        let source_identity = directory_identity(&source)?;
        if is_mountpoint(target)? {
            let current_is_expected = directory_identity(target)
                .is_ok_and(|identity| identity == source_identity)
                && tmp_directory_is_usable(target);
            if current_is_expected {
                return Ok(());
            }
            umount2(target, MntFlags::MNT_DETACH).with_context(|| {
                format!(
                    "failed to detach stale workspace /tmp mount at {}",
                    target.display()
                )
            })?;
        }
        mount_workspace_source(old_root, &workspace_tmp, target, workspace_idmap_option)
            .with_context(|| {
                format!(
                    "failed to mount disk-backed workspace /tmp from {} to {}",
                    workspace_tmp.display(),
                    target.display()
                )
            })?;
        crate::perf::record_mount();
        return verify_workspace_tmp_mount(target, Some(source_identity));
    }
    if is_mountpoint(target)? {
        if tmp_directory_is_usable(target) {
            return Ok(());
        }
        umount2(target, MntFlags::MNT_DETACH).with_context(|| {
            format!(
                "failed to detach stale workspace /tmp mount at {}",
                target.display()
            )
        })?;
    }
    mount(
        Some("tmpfs"),
        target,
        Some("tmpfs"),
        workspace_tmp_mount_flags(),
        Some(WORKSPACE_TMP_DATA),
    )
    .with_context(|| {
        format!(
            "failed to mount private workspace tmpfs at {}",
            target.display()
        )
    })?;
    crate::perf::record_mount();
    verify_workspace_tmp_mount(target, None)
}

pub(crate) fn runtime_tmpfs_mount_flags() -> MsFlags {
    MsFlags::MS_NODEV | MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC
}

pub(crate) const RUNTIME_TMPFS_DATA: &str = "mode=700";

pub(crate) const WORKSPACE_TMP_DATA: &str = "mode=1777";

pub(crate) fn workspace_tmp_mount_flags() -> MsFlags {
    MsFlags::MS_NODEV | MsFlags::MS_NOSUID
}

pub(crate) fn ensure_workspace_tmp_source(old_root: &Path, workspace_tmp: &Path) -> Result<()> {
    let source = path_inside_old_root(old_root, workspace_tmp)?;
    fs::create_dir_all(&source)
        .with_context(|| format!("failed to create {}", source.display()))?;
    let metadata = fs::symlink_metadata(&source).with_context(|| {
        format!(
            "failed to inspect workspace tmp source {}",
            source.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "workspace tmp source must be a real directory: {}",
            source.display()
        );
    }
    fs::set_permissions(&source, fs::Permissions::from_mode(0o1777))
        .with_context(|| format!("failed to chmod {}", source.display()))?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirectoryIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

pub(crate) fn directory_identity(path: &Path) -> Result<DirectoryIdentity> {
    let metadata = fs::metadata(path).with_context(|| {
        format!(
            "failed to inspect workspace tmp directory {}",
            path.display()
        )
    })?;
    if !metadata.is_dir() {
        bail!("workspace tmp path is not a directory: {}", path.display());
    }
    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

pub(crate) fn tmp_directory_is_usable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_dir() || metadata.nlink() < 2 || metadata.mode() & 0o1777 != 0o1777 {
        return false;
    }
    let probe = path.join(format!(
        ".enclave-tmp-probe-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    match OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(file) => {
            drop(file);
            fs::remove_file(probe).is_ok()
        }
        Err(_) => false,
    }
}

pub(crate) fn verify_workspace_tmp_mount(
    target: &Path,
    expected_identity: Option<DirectoryIdentity>,
) -> Result<()> {
    let identity = directory_identity(target)?;
    if expected_identity.is_some_and(|expected| expected != identity) {
        bail!(
            "workspace /tmp mount at {} does not reference its configured backing directory",
            target.display()
        );
    }
    if !tmp_directory_is_usable(target) {
        bail!(
            "workspace /tmp at {} is not a linked, writable directory with mode 1777",
            target.display()
        );
    }
    Ok(())
}
