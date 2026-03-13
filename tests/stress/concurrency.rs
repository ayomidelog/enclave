use std::fs;
use std::path::{Path, PathBuf};

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};

fn root_only() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn state_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create state dir");
    dir
}

fn prepare_cached_rootfs(state_dir: &Path, suite: &str) {
    let cache = state_dir.join("sandboxes").join("rootfs-cache").join(suite);
    fs::create_dir_all(cache.join("bin")).expect("create bin");
    fs::create_dir_all(cache.join("etc")).expect("create etc");
    fs::create_dir_all(cache.join("usr")).expect("create usr");
    fs::write(cache.join("bin/sh"), "#!/bin/sh\nexit 0\n").expect("write shell");
}

#[test]
#[ignore = "stress test requiring root privileges"]
fn concurrent_sandbox_lifecycle_operations_are_stable() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-stress-concurrent");
    prepare_cached_rootfs(&state, "bookworm");

    let mut sandboxes = Vec::new();
    for i in 0..4 {
        let sandbox = create_sandbox(
            &state,
            "debootstrap",
            &format!("concurrent-{i}"),
            "bookworm",
            "http://deb.debian.org/debian",
            &BootstrapMethod::CachedRootfs,
        )
        .expect("create sandbox");
        start_sandbox(&state, &sandbox.id).expect("start sandbox");
        sandboxes.push(sandbox);
    }

    for sandbox in &sandboxes {
        stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
        destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    }

    let _ = fs::remove_dir_all(state);
}
