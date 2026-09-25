//! Removing a directory repair has decided nothing owns.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Remove a directory repair found no owner for.
///
/// A directory that still has a mount at or below it is not a leftover: the
/// mount is holding something, and removing the directory underneath it would
/// either fail or leave the mount pointing at a path that no longer exists.
pub(super) fn remove_orphan_directory(path: &Path) -> Result<()> {
    if path_contains_mount(path)? {
        bail!(
            "refusing to remove orphaned directory {} while it or a descendant is still mounted",
            path.display()
        );
    }
    fs::remove_dir_all(path)
        .with_context(|| format!("failed to remove orphaned directory {}", path.display()))
}

fn path_contains_mount(path: &Path) -> Result<bool> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")
        .context("failed to read /proc/self/mountinfo")?;
    Ok(mountinfo
        .lines()
        .filter_map(mountinfo_path)
        .any(|mountpoint| mountpoint == path || mountpoint.starts_with(path)))
}

fn mountinfo_path(line: &str) -> Option<PathBuf> {
    let raw = line.split_whitespace().nth(4)?;
    Some(PathBuf::from(unescape_mountinfo_path(raw)))
}

/// Decode the octal escapes the kernel uses for space, tab, and newline in a
/// mountinfo path, so a mountpoint under a directory with a space in its name is
/// still matched.
fn unescape_mountinfo_path(path: &str) -> String {
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
