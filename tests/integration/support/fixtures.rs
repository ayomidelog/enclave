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
    fs::copy("/usr/bin/busybox", cache.join("bin").join("busybox")).expect("copy busybox");
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
