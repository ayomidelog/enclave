//! Running more than one command in a workspace at the same time.
//!
//! An interactive command such as `htop` must not make every later probe wait for
//! it to exit. This drives the library path directly, with two independent threads,
//! so it exercises the persistent-helper pool rather than only the daemon queue.

use std::fs;
use std::time::{Duration, Instant};

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    cancel_workspace_command, create_workspace, destroy_workspace, exec_workspace_command,
    start_workspace, stop_workspace, WorkspaceLimits,
};

use super::support::{prepare_cached_rootfs, root_only, state_dir};

fn persistent_helpers_for(workspace_id: &str) -> Vec<u32> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            let cmdline = fs::read(entry.path().join("cmdline")).ok()?;
            let cmdline = String::from_utf8_lossy(&cmdline);
            (cmdline.contains("workspace-session-persistent-helper")
                && cmdline.contains(workspace_id))
            .then_some(pid)
        })
        .collect()
}

#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_second_command_runs_while_a_first_command_is_still_running() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-concurrent-exec");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-concurrent-exec",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");

    let (first, second) = std::thread::scope(|scope| {
        let first_state = &state;
        let first_sandbox = &sandbox.id;
        let first_workspace = &workspace.id;
        let first = scope.spawn(move || {
            exec_workspace_command(
                first_state,
                first_sandbox,
                first_workspace,
                "/home",
                &[
                    "sh".into(),
                    "-c".into(),
                    "echo first-started; /bin/busybox sleep 30; echo first-finished".into(),
                ],
            )
        });
        // Wait until the first command's helper exists. The command itself is
        // sleeping, so its thread has not returned before cancellation.
        let deadline = Instant::now() + Duration::from_secs(10);
        while persistent_helpers_for(&workspace.id).is_empty() {
            assert!(
                Instant::now() < deadline,
                "the first command did not start a helper"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        let second_started = Instant::now();
        let second = exec_workspace_command(
            &state,
            &sandbox.id,
            &workspace.id,
            "/home",
            &["sh".into(), "-c".into(), "echo second-finished".into()],
        );
        let second_elapsed = second_started.elapsed();
        assert!(
            second_elapsed < Duration::from_secs(5),
            "the second command took {second_elapsed:?} while the first was sleeping"
        );
        assert!(
            !first.is_finished(),
            "the first command exited before the second could overlap it"
        );

        let cancelled = cancel_workspace_command(std::path::Path::new(&workspace.workspace_path))
            .expect("cancel the first command");
        assert!(
            cancelled.cancelled,
            "the running first command was not cancelled"
        );

        (
            first
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            second,
        )
    });

    let second = second.expect("the second command should complete");
    assert_eq!(second.exit_code, 0, "stderr={}", second.stderr);
    assert!(second.stdout.contains("second-finished"));
    assert!(
        first.is_err(),
        "the cancelled first command should report a helper failure"
    );
    assert!(
        !persistent_helpers_for(&workspace.id).is_empty(),
        "the concurrent commands should reuse the workspace helper pool"
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    assert!(
        persistent_helpers_for(&workspace.id).is_empty(),
        "the stop left persistent helpers behind"
    );
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
