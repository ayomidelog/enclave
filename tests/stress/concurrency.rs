use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

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

/// Four concurrent lifecycles over one shared host, with the addresses compared
/// while every workspace is running.
///
/// The property is that two workspaces which are alive at the same time never
/// share an address, which is what the reservation under the registry lock exists
/// to guarantee. An address a stopped workspace no longer holds is free for the
/// next one, so comparing addresses captured at different moments would report
/// that recycling as a collision.
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
        // Two barriers, because there are two things to make simultaneous: the
        // reservation, so the four threads contend for the registry lock rather than
        // reserving one after another, and the comparison, so the addresses are read
        // while every workspace is still running.
        let about_to_start = Arc::new(Barrier::new(4));
        let started_together = Arc::new(Barrier::new(4));
        let handles = (0..4)
            .map(|index| {
                let state = state.clone();
                let about_to_start = Arc::clone(&about_to_start);
                let started_together = Arc::clone(&started_together);
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
                    // The reservation happens under the registry lock, and this is
                    // where four threads contend for it. Waiting here makes all four
                    // reserve at once instead of whenever each finished its sandbox,
                    // which is what the property under test is about.
                    about_to_start.wait();
                    let started = start_workspace(&state, &sandbox.id, &workspace.id)
                        .unwrap_or_else(|error| panic!("start workspace in {name}: {error:#}"));
                    let ip = started.assigned_ip.clone().unwrap_or_else(|| {
                        panic!("workspace in {name} started without an address")
                    });
                    // And the address is captured with all four still running. The
                    // earlier version of this test compared the addresses as each
                    // thread finished, so a thread that released its address before a
                    // later one reserved it made the two look like a collision. That
                    // is recycling, not a collision: an address a stopped workspace no
                    // longer holds is free for the next one.
                    started_together.wait();
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
            "two workspaces that were running at the same time were given the same \
             address; got {addresses:?}"
        );
    });

    let _ = fs::remove_dir_all(state);
}
