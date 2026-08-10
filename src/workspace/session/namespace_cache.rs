use std::collections::HashMap;
use std::fs::File;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};

use super::{process::read_namespace_identity, process_matches};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CacheKey {
    pid: u32,
    starttime_ticks: u64,
}

#[derive(Debug)]
struct CachedHandles {
    identity: [String; 5],
    root: File,
    user: File,
    mount: File,
    pid: File,
    net: File,
    uts: File,
}

#[derive(Debug)]
pub(crate) struct ChildNamespaceFds {
    pub(crate) root: File,
    pub(crate) user: File,
    pub(crate) mount: File,
    pub(crate) pid: File,
    pub(crate) net: File,
    pub(crate) uts: File,
}

static CACHE: OnceLock<Mutex<HashMap<CacheKey, CachedHandles>>> = OnceLock::new();
const CACHE_LIMIT: usize = 256;

pub(crate) fn duplicate_for_child(pid: u32, starttime_ticks: u64) -> Result<ChildNamespaceFds> {
    if !process_matches(pid, Some(starttime_ticks)) {
        anyhow::bail!(
            "workspace runtime pid {pid} is not alive or no longer matches expected start time"
        );
    }
    let identity = read_namespace_identity(pid)?;
    let key = CacheKey {
        pid,
        starttime_ticks,
    };
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("namespace descriptor cache lock poisoned"))?;
    if !guard.contains_key(&key) {
        crate::perf::record_namespace_cache_miss();
        guard.insert(key, CachedHandles::open(pid, identity.clone())?);
    } else if guard
        .get(&key)
        .is_some_and(|entry| entry.identity != identity)
    {
        crate::perf::record_namespace_cache_miss();
        guard.insert(key, CachedHandles::open(pid, identity)?);
    } else {
        crate::perf::record_namespace_cache_hit();
    }
    if guard.len() > CACHE_LIMIT {
        if let Some(oldest) = guard.keys().next().copied() {
            guard.remove(&oldest);
        }
    }
    guard
        .get(&key)
        .context("namespace descriptor cache entry disappeared")?
        .duplicate()
}

pub(crate) fn invalidate(pid: u32, starttime_ticks: Option<u64>) {
    let Some(starttime_ticks) = starttime_ticks else {
        return;
    };
    if let Some(cache) = CACHE.get() {
        if let Ok(mut guard) = cache.lock() {
            guard.remove(&CacheKey {
                pid,
                starttime_ticks,
            });
        }
    }
}

impl CachedHandles {
    fn open(pid: u32, identity: [String; 5]) -> Result<Self> {
        Ok(Self {
            identity,
            root: File::open(format!("/proc/{pid}/root"))
                .with_context(|| format!("failed to open /proc/{pid}/root"))?,
            user: File::open(format!("/proc/{pid}/ns/user"))
                .with_context(|| format!("failed to open /proc/{pid}/ns/user"))?,
            mount: File::open(format!("/proc/{pid}/ns/mnt"))
                .with_context(|| format!("failed to open /proc/{pid}/ns/mnt"))?,
            pid: File::open(format!("/proc/{pid}/ns/pid"))
                .with_context(|| format!("failed to open /proc/{pid}/ns/pid"))?,
            net: File::open(format!("/proc/{pid}/ns/net"))
                .with_context(|| format!("failed to open /proc/{pid}/ns/net"))?,
            uts: File::open(format!("/proc/{pid}/ns/uts"))
                .with_context(|| format!("failed to open /proc/{pid}/ns/uts"))?,
        })
    }

    fn duplicate(&self) -> Result<ChildNamespaceFds> {
        Ok(ChildNamespaceFds {
            root: duplicate_inheritable(&self.root)?,
            user: duplicate_inheritable(&self.user)?,
            mount: duplicate_inheritable(&self.mount)?,
            pid: duplicate_inheritable(&self.pid)?,
            net: duplicate_inheritable(&self.net)?,
            uts: duplicate_inheritable(&self.uts)?,
        })
    }
}

fn duplicate_inheritable(file: &File) -> Result<File> {
    let duplicate = file
        .try_clone()
        .context("failed to duplicate namespace descriptor")?;
    let fd = duplicate.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to inspect namespace descriptor flags");
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to make namespace descriptor inheritable");
    }
    Ok(duplicate)
}

pub(crate) fn raw_fds(fds: &ChildNamespaceFds) -> [RawFd; 6] {
    [
        fds.root.as_raw_fd(),
        fds.user.as_raw_fd(),
        fds.mount.as_raw_fd(),
        fds.pid.as_raw_fd(),
        fds.net.as_raw_fd(),
        fds.uts.as_raw_fd(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::session::process_starttime_ticks;

    #[test]
    fn descriptor_cache_duplicates_identity_checked_handles() {
        let pid = std::process::id();
        let starttime = process_starttime_ticks(pid).expect("current process start time");
        let fds = duplicate_for_child(pid, starttime).expect("duplicate current namespaces");
        for fd in raw_fds(&fds) {
            assert!(fd >= 0);
        }
    }
}
