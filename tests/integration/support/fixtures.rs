//! Building the state a privileged test needs: a cached rootfs, a state
//! directory, and a cleanup guard.

use std::fs;
use std::path::{Path, PathBuf};

use enclave::sandbox::destroy_sandbox;

pub(crate) fn prepare_cached_rootfs(state_dir: &Path, suite: &str) {
    let cache = state_dir.join("sandboxes").join("rootfs-cache").join(suite);
    for directory in ["bin", "etc", "opt", "usr/bin"] {
        fs::create_dir_all(cache.join(directory)).expect("create rootfs directory");
    }
    fs::copy(busybox_path(), cache.join("bin").join("busybox")).expect("copy busybox");
    // Relative links, so the fixture does not depend on the workspace root being
    // the process root when an applet is resolved.
    for (link, target) in [
        ("bin/sh", "busybox"),
        ("bin/cat", "busybox"),
        ("bin/dd", "busybox"),
        ("usr/bin/env", "../../bin/busybox"),
    ] {
        std::os::unix::fs::symlink(target, cache.join(link)).expect("link busybox applet");
    }
}

/// Where the static shell the fixture rootfs is built from lives.
///
/// The fixture needs a shell that runs inside a workspace with no shared
/// libraries, so it cannot use the host's `/bin/sh`: that one is dynamically
/// linked against libraries the workspace rootfs does not have. Busybox is the
/// smallest thing that provides one, and the search path covers the two places a
/// distribution puts it so the suite does not depend on which package a host
/// installed. A host without it is reported with what to install rather than as
/// a bare copy failure.
fn busybox_path() -> &'static Path {
    const CANDIDATES: [&str; 3] = ["/usr/bin/busybox", "/bin/busybox", "/usr/local/bin/busybox"];
    for candidate in CANDIDATES {
        let path = Path::new(candidate);
        if path.is_file() {
            return path;
        }
    }
    panic!(
        "the integration fixture builds a rootfs from busybox, and none of {CANDIDATES:?} exists. \
         Install it (Debian/Ubuntu: 'apt-get install busybox-static') and run the suite again."
    );
}

pub(crate) fn state_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create state dir");
    dir
}

pub(crate) struct SandboxCleanup {
    state_dir: PathBuf,
    sandbox_id: Option<String>,
}

impl SandboxCleanup {
    pub(crate) fn new(state_dir: PathBuf) -> Self {
        Self {
            state_dir,
            sandbox_id: None,
        }
    }

    pub(crate) fn record(&mut self, sandbox_id: &str) {
        self.sandbox_id = Some(sandbox_id.to_string());
    }
}

impl Drop for SandboxCleanup {
    fn drop(&mut self) {
        if let Some(sandbox_id) = self.sandbox_id.as_deref() {
            let _ = destroy_sandbox(&self.state_dir, sandbox_id);
        }
        let _ = fs::remove_dir_all(&self.state_dir);
    }
}
