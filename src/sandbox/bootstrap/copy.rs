//! Copying a rootfs tree, the fallback when a shared base is unavailable.

use super::*;

pub(crate) fn has_rootfs_content(dir: &Path) -> bool {
    ["bin", "etc", "usr"]
        .iter()
        .all(|required| dir.join(required).is_dir())
}

/// Copy a cached rootfs tree into a sandbox rootfs directory.
///
/// This is the fallback for hosts where the shared-base overlay cannot be set
/// up, and it is also how a freshly bootstrapped rootfs is published to the
/// cache.
pub(crate) fn copy_cached_rootfs(src: &Path, dst: &Path) -> Result<()> {
    copy_dir_recursive(src, dst).with_context(|| {
        format!(
            "failed to copy cached rootfs from {} to {}",
            src.display(),
            dst.display()
        )
    })
}

pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
    }

    validate_copy_source(src)?;

    let src_arg = format!("{}/.", src.display());
    HostCommand::new("cp")
        .args([
            "-a",
            "--reflink=auto",
            &src_arg,
            dst.to_string_lossy().as_ref(),
        ])
        .timeout(ROOTFS_COPY_TIMEOUT)
        .run_checked()
        .with_context(|| format!("failed to run cp -a {} {}", src.display(), dst.display()))?;
    Ok(())
}

pub(crate) fn validate_copy_source(src: &Path) -> Result<()> {
    for entry in fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();

        if file_type.is_dir() {
            validate_copy_source(&src_path)?;
        } else if file_type.is_symlink() {
            fs::read_link(&src_path)
                .with_context(|| format!("failed to read symlink {}", src_path.display()))?;
        } else if !(file_type.is_file()
            || file_type.is_fifo()
            || file_type.is_char_device()
            || file_type.is_block_device())
        {
            bail!(
                "unsupported filesystem entry type for {}",
                src_path.display()
            );
        }
    }

    Ok(())
}
