use super::*;

pub(crate) fn open_host_directory(path: &Path) -> Result<File> {
    if !path.is_absolute() {
        bail!(
            "host destination parent must be absolute: {}",
            path.display()
        );
    }
    let mut current = File::open("/").context("failed to open host root directory")?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => {
                let name = cstring_os(name)?;
                let descriptor = unsafe {
                    libc::openat(
                        current.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if descriptor < 0 {
                    return Err(std::io::Error::last_os_error()).with_context(|| {
                        format!(
                            "failed to securely open host destination directory {}",
                            path.display()
                        )
                    });
                }
                current = unsafe { File::from_raw_fd(descriptor) };
            }
            Component::ParentDir | Component::Prefix(_) => {
                bail!(
                    "host destination parent contains unsafe component: {}",
                    path.display()
                )
            }
        }
    }
    Ok(current)
}

pub(crate) fn ensure_host_entry_absent(parent: &File, name: &str) -> Result<()> {
    let name = cstring(name)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        bail!(
            "host destination '{}' already exists",
            name.to_string_lossy()
        );
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        return Ok(());
    }
    Err(error).context("failed to inspect host destination entry")
}

pub(crate) fn cstring(value: &str) -> Result<CString> {
    CString::new(value).map_err(|_| anyhow!("path contains an interior NUL byte"))
}

pub(crate) fn cstring_os(value: &std::ffi::OsStr) -> Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| anyhow!("path contains an interior NUL byte"))
}
