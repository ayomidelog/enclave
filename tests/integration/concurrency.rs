//! What a stop and a start racing on one workspace are allowed to leave behind.
//!
//! The daemon serializes these two with a workspace lease, so the library has to
//! be safe under the narrower guarantee that the registry lock gives it: the
//! mutations are ordered, the host work is not. The test drives the pair from two
//! threads to pin the outcome the rest of the system relies on, which is that
//! whichever order they land in, the record afterwards describes the host.

use std::path::Path;

use enclave::sandbox::{create_sandbox, start_sandbox, stop_sandbox, BootstrapMethod};
use enclave::workspace::{
    create_workspace, destroy_workspace, start_workspace, stop_workspace, WorkspaceLimits,
};

use super::support::{
    cgroup_processes, prepare_cached_rootfs, process_starttime, root_only, state_dir,
    workspace_cgroup_path, SandboxCleanup,
};

struct Recorded {
    status: String,
    pid: Option<u32>,
    starttime_ticks: Option<u64>,
    assigned_ip: Option<String>,
}

fn workspace_record(state: &Path, workspace_id: &str) -> Recorded {
    enclave::registry::with_registry(state, |registry| {
        let workspace = registry
            .sandboxes
            .values()
            .find_map(|sandbox| sandbox.workspaces.get(workspace_id))
            .expect("the test's workspace record is gone");
        Ok(Recorded {
            status: workspace.status.as_str().to_string(),
            pid: workspace.runtime_pid,
            starttime_ticks: workspace.runtime_starttime_ticks,
            assigned_ip: workspace.assigned_ip.clone(),
        })
    })
    .expect("read the registry")
}

fn outcome<T>(result: &Result<T, anyhow::Error>) -> &'static str {
    if result.is_ok() {
        "settled"
    } else {
        "refused"
    }
}

/// The record must describe a host state that is one of the two settled ones.
fn assert_record_describes_the_host(
    state: &Path,
    sandbox_id: &str,
    workspace_id: &str,
    round: usize,
) {
    let record = workspace_record(state, workspace_id);
    let cgroup = workspace_cgroup_path(sandbox_id, workspace_id);
    match record.status.as_str() {
        "running" => {
            let pid = record.pid.unwrap_or_else(|| {
                panic!("round {round}: a running workspace has no recorded pid")
            });
            assert_eq!(
                process_starttime(pid),
                record.starttime_ticks,
                "round {round}: the record names a process that is not running with its start time"
            );
            assert!(
                cgroup.exists(),
                "round {round}: a running workspace has no cgroup"
            );
            assert!(
                cgroup_processes(&cgroup)
                    .iter()
                    .any(|(held, _)| *held == pid),
                "round {round}: the workspace runtime is not in its cgroup"
            );
            assert!(
                record.assigned_ip.is_some(),
                "round {round}: a running workspace holds no address"
            );
        }
        "stopped" => {
            assert_eq!(
                record.pid, None,
                "round {round}: a stopped workspace still records a runtime"
            );
            assert_eq!(
                record.assigned_ip, None,
                "round {round}: a stopped workspace still holds an address"
            );
            assert!(
                !cgroup.exists(),
                "round {round}: a stopped workspace still has a cgroup"
            );
        }
        other => panic!("round {round}: the race left the workspace {other}"),
    }
}

/// A stop and a start of one workspace, started together, must not leave a
/// transitional record or one that disagrees with the host.
///
/// Both calls may not succeed: a start that arrives while the stop is tearing the
/// runtime down is refused rather than resumed, and a stop that arrives during a
/// launch is refused the same way. That is the contract, and the point of the test
/// is that the refusal is clean. A record left transitional would block every later
/// start until a repair rolled it back, and a record that claimed a runtime the host
/// does not have would make the next stop signal nothing.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_stop_and_a_start_racing_on_one_workspace_settle() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-stop-start-race");
    prepare_cached_rootfs(&state, "bookworm");
    let mut cleanup = SandboxCleanup::new(state.clone());
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-race-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    cleanup.record(&sandbox.id);
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");

    // A handful of rounds, because which of the two settles is decided by the
    // scheduler and one round only ever shows one of the orders. The fixture is one
    // workspace, so this stays cheap.
    for round in 0..4 {
        let (stopped, started) = std::thread::scope(|scope| {
            let stop = scope.spawn(|| stop_workspace(&state, &sandbox.id, &workspace.id));
            let start = scope.spawn(|| start_workspace(&state, &sandbox.id, &workspace.id));
            (
                stop.join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                start
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            )
        });

        // The daemon serializes these two with a workspace lease, so the library
        // only owes callers the narrower promise that a race cannot corrupt the
        // record or strand the workspace. It does not owe them success: the two
        // operations overlap in host work that they cannot order, so either may be
        // refused, or may lose the runtime it launched, and report that. What
        // neither may do is leave a record that disagrees with the host, which is
        // what the check below is about. A start that fails partway is the case
        // worth having: it is the one that rolls a reservation back, and a rollback
        // that raced a teardown is what would leave a workspace stranded.
        assert_record_describes_the_host(&state, &sandbox.id, &workspace.id, round);
        eprintln!(
            "round {round}: stop {}, start {}",
            outcome(&stopped),
            outcome(&started)
        );
    }

    // The race must not leave the workspace unusable: a plain start brings it back
    // to a settled running state, and a plain stop then cleans the host.
    let restarted = start_workspace(&state, &sandbox.id, &workspace.id)
        .expect("the workspace must still start after the race");
    assert_eq!(restarted.status.as_str(), "running");
    assert!(
        restarted.runtime_pid.is_some(),
        "the workspace started after the race without a runtime"
    );

    // And a stop still settles it, with the host clean.
    let stopped = stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    assert_eq!(stopped.status.as_str(), "stopped");
    assert_eq!(stopped.runtime_pid, None);
    assert!(
        !workspace_cgroup_path(&sandbox.id, &workspace.id).exists(),
        "the workspace still has a cgroup after a clean stop"
    );

    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    drop(cleanup);
}
