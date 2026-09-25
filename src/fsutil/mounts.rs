use std::ffi::{CString, OsStr};
use std::fs::{self};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The filesystem type OverlayFS mounts report.
const OVERLAY_FILESYSTEM: &str = "overlay";

/// One mount, reduced to the fields Enclave needs to identify it.
///
/// `/proc/self/mountinfo` numbers its fields from one: the mount root is field
/// four, the mount point field five, the optional fields field seven, and then
/// `fstype` and the mount source follow the ` - ` separator. The root is what a
/// bind mount reports as the directory it was bound from, and the source is the
/// device or filesystem name, which for a bind mount is the underlying device
/// rather than the bound directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MountInfoEntry {
    /// The directory inside the mounted filesystem the mount starts at.
    pub(crate) root: PathBuf,
    /// Where the mount appears in this mount namespace.
    pub(crate) mountpoint: PathBuf,
    /// The filesystem type.
    pub(crate) filesystem_type: String,
    /// The device or filesystem the mount came from.
    pub(crate) source: PathBuf,
}

impl MountInfoEntry {
    /// Whether Enclave created this mount.
    ///
    /// Enclave mounts exactly three kinds of thing inside its state directory,
    /// and each is recognised from the mount itself rather than from its path, so
    /// a mount an operator placed under a workspace is never mistaken for
    /// Enclave's own:
    ///
    /// * the sandbox rootfs and the workspace home and root overlays, which are
    ///   OverlayFS mounts;
    /// * bind mounts of a directory inside the state tree, such as the sandbox
    ///   rootfs bind mount, whose mount root lies inside the state directory;
    /// * the workspace disk images, which are loop devices backed by a file
    ///   inside the state directory.
    ///
    /// Anything else is foreign. Enclave refuses to unmount a foreign mount,
    /// because the data behind it is the operator's and detaching it is not
    /// something Enclave can undo.
    pub(crate) fn is_enclave_owned(&self, state_dir: &Path) -> bool {
        self.filesystem_type == OVERLAY_FILESYSTEM
            || self.root.starts_with(state_dir)
            || loop_backing_file(&self.source).is_some_and(|backing| backing.starts_with(state_dir))
    }

    /// `<mountpoint> (source <source>)`, the form Enclave's reports use.
    pub(crate) fn describe(&self) -> String {
        format!(
            "{} (source {})",
            self.mountpoint.display(),
            self.source.display()
        )
    }
}

pub(crate) struct MountInfoSnapshot {
    entries: Vec<MountInfoEntry>,
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
            entries: raw.lines().filter_map(parse_mountinfo_line).collect(),
        }
    }

    pub(crate) fn contains(&self, path: &Path) -> bool {
        self.entries.iter().any(|entry| entry.mountpoint == path)
    }

    /// Every mount at or below `root`, deepest first.
    pub(crate) fn at_or_below_entries(&self, root: &Path) -> Vec<&MountInfoEntry> {
        let mut entries = self
            .entries
            .iter()
            .filter(|entry| entry.mountpoint == root || entry.mountpoint.starts_with(root))
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.mountpoint.components().count()));
        entries.dedup_by(|left, right| left.mountpoint == right.mountpoint);
        entries
    }

    /// The mount points at or below `root`, deepest first.
    #[cfg(test)]
    pub(crate) fn at_or_below(&self, root: &Path) -> Vec<PathBuf> {
        self.at_or_below_entries(root)
            .into_iter()
            .map(|entry| entry.mountpoint.clone())
            .collect()
    }

    /// The mounts at or below `root` that Enclave did not create, described for
    /// a report. When `root` does not follow an Enclave layout there is no state
    /// directory to prove ownership against, so every mount is reported.
    pub(crate) fn foreign_at_or_below(&self, root: &Path) -> Vec<String> {
        let state_dir = enclave_state_root(root);
        self.at_or_below_entries(root)
            .into_iter()
            .filter(|entry| {
                !state_dir
                    .as_deref()
                    .is_some_and(|state_dir| entry.is_enclave_owned(state_dir))
            })
            .map(MountInfoEntry::describe)
            .collect()
    }

    /// The mounts at or below `root` that Enclave created, described for a
    /// report. When `root` does not follow an Enclave layout nothing can be
    /// attributed to Enclave, so the result is empty.
    pub(crate) fn owned_at_or_below(&self, root: &Path) -> Vec<String> {
        let Some(state_dir) = enclave_state_root(root) else {
            return Vec::new();
        };
        self.at_or_below_entries(root)
            .into_iter()
            .filter(|entry| entry.is_enclave_owned(&state_dir))
            .map(MountInfoEntry::describe)
            .collect()
    }
}

/// The Enclave state directory that owns `path`, when `path` follows one of the
/// layouts Enclave creates.
///
/// Every path Enclave manages sits at or below `<state>/sandboxes`: the sandboxes
/// directory itself, a sandbox directory, a workspace directory, and everything
/// inside them. The nearest `sandboxes` component in the path is therefore the
/// anchor, and its parent is the state directory that owns the path. A path with
/// no `sandboxes` component has no provable owner, and callers then leave the
/// mounts alone instead of guessing.
pub(crate) fn enclave_state_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| ancestor.file_name().and_then(OsStr::to_str) == Some("sandboxes"))?
        .parent()
        .map(Path::to_path_buf)
}

/// The file a loop device is backed by, when `source` names one.
///
/// The kernel reports the backing path relative to the filesystem root, so a path
/// without a leading separator is anchored at `/`. An unreadable or empty
/// backing file means the device cannot be attributed to Enclave, and the mount
/// is then treated as foreign.
fn loop_backing_file(source: &Path) -> Option<PathBuf> {
    if source.parent() != Some(Path::new("/dev")) {
        return None;
    }
    let name = source.file_name()?.to_str()?;
    let number = name.strip_prefix("loop")?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let raw = fs::read_to_string(format!("/sys/block/{name}/loop/backing_file")).ok()?;
    let raw = raw.trim_end_matches('\n');
    if raw.is_empty() {
        return None;
    }
    let backing = Path::new(raw);
    Some(if backing.is_absolute() {
        backing.to_path_buf()
    } else {
        Path::new("/").join(backing)
    })
}

fn parse_mountinfo_line(line: &str) -> Option<MountInfoEntry> {
    let (before, after) = line.split_once(" - ")?;
    let mut before = before.split_whitespace();
    let root = before.nth(3)?;
    let mountpoint = before.next()?;
    let mut after = after.split_whitespace();
    let filesystem_type = after.next()?;
    let source = after.next()?;
    Some(MountInfoEntry {
        root: PathBuf::from(unescape_mountinfo_path(root)),
        mountpoint: PathBuf::from(unescape_mountinfo_path(mountpoint)),
        filesystem_type: filesystem_type.to_string(),
        source: PathBuf::from(unescape_mountinfo_path(source)),
    })
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
