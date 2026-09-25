use std::fs;
use std::path::{Path, PathBuf};

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, destroy_workspace, start_workspace, stop_workspace, WorkspaceLimits,
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

    // Four sandboxes, each with one running workspace, created, started, stopped,
    // and destroyed at the same time. The point is the shared host state: one
    // bridge, one NAT rule set, one registry, and one set of IP allocations, all
    // mutated by every thread. A sequential loop would not exercise any of that.
    std::thread::scope(|scope| {
        let handles = (0..4)
            .map(|index| {
                let state = state.clone();
                scope.spawn(move || {
                    let name = format!("concurrent-{index}");
                    let sandbox = create_sandbox(
                        &state,
                        "debootstrap",
                        &name,
                        "bookworm",
                        "http://deb.debian.org/debian",
                        &BootstrapMethod::CachedRootfs,
                    )
                    .unwrap_or_else(|error| panic!("create {name}: {error:#}"));
                    start_sandbox(&state, &sandbox.id)
                        .unwrap_or_else(|error| panic!("start {name}: {error:#}"));
                    let workspace =
                        create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
                            .unwrap_or_else(|error| {
                                panic!("create workspace in {name}: {error:#}")
                            });
                    let started = start_workspace(&state, &sandbox.id, &workspace.id)
                        .unwrap_or_else(|error| panic!("start workspace in {name}: {error:#}"));
                    // Every workspace must have taken a distinct address, or two
                    // of them would share one.
                    let ip = started.assigned_ip.clone().unwrap_or_else(|| {
                        panic!("workspace in {name} started without an address")
                    });
                    stop_workspace(&state, &sandbox.id, &workspace.id)
                        .unwrap_or_else(|error| panic!("stop workspace in {name}: {error:#}"));
                    destroy_workspace(&state, &sandbox.id, &workspace.id)
                        .unwrap_or_else(|error| panic!("destroy workspace in {name}: {error:#}"));
                    stop_sandbox(&state, &sandbox.id)
                        .unwrap_or_else(|error| panic!("stop {name}: {error:#}"));
                    destroy_sandbox(&state, &sandbox.id)
                        .unwrap_or_else(|error| panic!("destroy {name}: {error:#}"));
                    ip
                })
            })
            .collect::<Vec<_>>();
        let addresses = handles
            .into_iter()
            .map(|handle| handle.join().expect("lifecycle thread panicked"))
            .collect::<Vec<_>>();
        let unique = addresses.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            unique.len(),
            addresses.len(),
            "each sandbox must take a distinct address; got {addresses:?}"
        );
    });

    let _ = fs::remove_dir_all(state);
}
