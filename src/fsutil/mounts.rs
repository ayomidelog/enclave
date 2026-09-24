use std::ffi::CString;
use std::fs::{self};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub(crate) struct MountInfoSnapshot {
    mountpoints: Vec<PathBuf>,
}

impl MountInfoSnapshot {
    pub(crate) fn load() -> Result<Self> {
        let raw = match fs::read_to_string("/proc/self/mountinfo") {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error).context("failed to read /proc/self/mountinfo"),
        };
        Ok(Self::parse(&raw))
    }

    pub(crate) fn parse(raw: &str) -> Self {
        Self {
            mountpoints: raw
                .lines()
                .filter_map(|line| line.split_whitespace().nth(4))
                .map(unescape_mountinfo_path)
                .map(PathBuf::from)
                .collect(),
        }
    }

    pub(crate) fn contains(&self, path: &Path) -> bool {
        self.mountpoints.iter().any(|mountpoint| mountpoint == path)
    }

    pub(crate) fn at_or_below(&self, root: &Path) -> Vec<PathBuf> {
        let mut mountpoints = self
            .mountpoints
            .iter()
            .filter(|mountpoint| *mountpoint == root || mountpoint.starts_with(root))
            .cloned()
            .collect::<Vec<_>>();
        mountpoints.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        mountpoints.dedup();
        mountpoints
    }
}

pub fn is_mountpoint(path: &Path) -> Result<bool> {
    Ok(MountInfoSnapshot::load()?.contains(path))
}

pub fn bind_mount(source: &Path, target: &Path) -> std::io::Result<()> {
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let target = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let result = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub fn make_mount_private(target: &Path) -> std::io::Result<()> {
    let target = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let result = unsafe {
        libc::mount(
            std::ptr::null(),
            target.as_ptr(),
            std::ptr::null(),
            libc::MS_PRIVATE,
            std::ptr::null(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub(crate) fn unescape_mountinfo_path(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..=index + 3].iter().all(u8::is_ascii_digit)
        {
            let value = (bytes[index + 1] - b'0') * 64
                + (bytes[index + 2] - b'0') * 8
                + (bytes[index + 3] - b'0');
            result.push(value as char);
            index += 4;
        } else {
            result.push(bytes[index] as char);
            index += 1;
        }
    }
    result
}
