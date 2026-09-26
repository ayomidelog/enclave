use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Barrier};
use std::thread;
use std::time::Duration;

use super::*;

/// Acquire a lease on another thread and report when it is held.
fn hold(
    leases: Arc<LifecycleLeases>,
    scope: LeaseScope,
    release: Arc<Barrier>,
    held: Arc<Barrier>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let _guard = leases.acquire(scope).expect("acquire lease");
        held.wait();
        release.wait();
    })
}

/// Try to acquire a lease and report whether it was granted promptly.
fn try_acquire(leases: Arc<LifecycleLeases>, scope: LeaseScope) -> bool {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let guard = leases.acquire(scope);
        let _ = sender.send(guard.is_ok());
        // Keep the guard alive until the caller has read the result.
        thread::sleep(Duration::from_millis(50));
        drop(guard);
    });
    receiver
        .recv_timeout(Duration::from_millis(150))
        .unwrap_or(false)
}

#[test]
fn unrelated_sandboxes_do_not_block_each_other() {
    let leases = Arc::new(LifecycleLeases::default());
    let release = Arc::new(Barrier::new(2));
    let held = Arc::new(Barrier::new(2));
    let first = hold(
        Arc::clone(&leases),
        LeaseScope::Sandbox("alpha".to_string()),
        Arc::clone(&release),
        Arc::clone(&held),
    );
    held.wait();

    assert!(try_acquire(
        Arc::clone(&leases),
        LeaseScope::Sandbox("beta".to_string())
    ));

    release.wait();
    first.join().unwrap();
}

#[test]
fn a_sandbox_wide_operation_blocks_another_in_the_same_sandbox() {
    let leases = Arc::new(LifecycleLeases::default());
    let release = Arc::new(Barrier::new(2));
    let held = Arc::new(Barrier::new(2));
    let first = hold(
        Arc::clone(&leases),
        LeaseScope::Sandbox("alpha".to_string()),
        Arc::clone(&release),
        Arc::clone(&held),
    );
    held.wait();

    assert!(!try_acquire(
        Arc::clone(&leases),
        LeaseScope::Sandbox("alpha".to_string())
    ));

    release.wait();
    first.join().unwrap();
}

#[test]
fn unrelated_workspaces_in_one_sandbox_do_not_block_each_other() {
    let leases = Arc::new(LifecycleLeases::default());
    let release = Arc::new(Barrier::new(2));
    let held = Arc::new(Barrier::new(2));
    let first = hold(
        Arc::clone(&leases),
        LeaseScope::Workspace {
            sandbox: "alpha".to_string(),
            workspace: "ws1".to_string(),
        },
        Arc::clone(&release),
        Arc::clone(&held),
    );
    held.wait();

    assert!(try_acquire(
        Arc::clone(&leases),
        LeaseScope::Workspace {
            sandbox: "alpha".to_string(),
            workspace: "ws2".to_string(),
        }
    ));

    release.wait();
    first.join().unwrap();
}

#[test]
fn the_same_workspace_blocks_itself() {
    let leases = Arc::new(LifecycleLeases::default());
    let release = Arc::new(Barrier::new(2));
    let held = Arc::new(Barrier::new(2));
    let scope = LeaseScope::Workspace {
        sandbox: "alpha".to_string(),
        workspace: "ws1".to_string(),
    };
    let first = hold(
        Arc::clone(&leases),
        scope.clone(),
        Arc::clone(&release),
        Arc::clone(&held),
    );
    held.wait();

    assert!(!try_acquire(Arc::clone(&leases), scope.clone()));

    release.wait();
    first.join().unwrap();
}

#[test]
fn a_sandbox_wide_operation_excludes_the_workspaces_in_it() {
    let leases = Arc::new(LifecycleLeases::default());
    let release = Arc::new(Barrier::new(2));
    let held = Arc::new(Barrier::new(2));
    let first = hold(
        Arc::clone(&leases),
        LeaseScope::Sandbox("alpha".to_string()),
        Arc::clone(&release),
        Arc::clone(&held),
    );
    held.wait();

    assert!(!try_acquire(
        Arc::clone(&leases),
        LeaseScope::Workspace {
            sandbox: "alpha".to_string(),
            workspace: "ws1".to_string(),
        }
    ));

    release.wait();
    first.join().unwrap();
}

#[test]
fn a_workspace_operation_excludes_a_sandbox_wide_one() {
    let leases = Arc::new(LifecycleLeases::default());
    let release = Arc::new(Barrier::new(2));
    let held = Arc::new(Barrier::new(2));
    let first = hold(
        Arc::clone(&leases),
        LeaseScope::Workspace {
            sandbox: "alpha".to_string(),
            workspace: "ws1".to_string(),
        },
        Arc::clone(&release),
        Arc::clone(&held),
    );
    held.wait();

    assert!(!try_acquire(
        Arc::clone(&leases),
        LeaseScope::Sandbox("alpha".to_string())
    ));

    release.wait();
    first.join().unwrap();
}

#[test]
fn the_global_scope_excludes_every_sandbox() {
    let leases = Arc::new(LifecycleLeases::default());
    let release = Arc::new(Barrier::new(2));
    let held = Arc::new(Barrier::new(2));
    let first = hold(
        Arc::clone(&leases),
        LeaseScope::Global,
        Arc::clone(&release),
        Arc::clone(&held),
    );
    held.wait();

    assert!(!try_acquire(
        Arc::clone(&leases),
        LeaseScope::Sandbox("alpha".to_string())
    ));
    assert!(!try_acquire(
        Arc::clone(&leases),
        LeaseScope::Workspace {
            sandbox: "beta".to_string(),
            workspace: "ws1".to_string(),
        }
    ));

    release.wait();
    first.join().unwrap();
}

#[test]
fn a_sandbox_wide_operation_excludes_the_global_scope() {
    let leases = Arc::new(LifecycleLeases::default());
    let release = Arc::new(Barrier::new(2));
    let held = Arc::new(Barrier::new(2));
    let first = hold(
        Arc::clone(&leases),
        LeaseScope::Sandbox("alpha".to_string()),
        Arc::clone(&release),
        Arc::clone(&held),
    );
    held.wait();

    assert!(!try_acquire(Arc::clone(&leases), LeaseScope::Global));

    release.wait();
    first.join().unwrap();
}

#[test]
fn a_released_lease_can_be_taken_again() {
    let leases = Arc::new(LifecycleLeases::default());
    let scope = LeaseScope::Workspace {
        sandbox: "alpha".to_string(),
        workspace: "ws1".to_string(),
    };
    {
        let _guard = leases.acquire(scope.clone()).expect("first acquire");
    }
    let _guard = leases.acquire(scope).expect("acquire after release");
}

#[test]
fn concurrent_holders_are_admitted_one_at_a_time() {
    let leases = Arc::new(LifecycleLeases::default());
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let leases = Arc::clone(&leases);
        let active = Arc::clone(&active);
        let peak = Arc::clone(&peak);
        handles.push(thread::spawn(move || {
            let _guard = leases
                .acquire(LeaseScope::Sandbox("alpha".to_string()))
                .expect("acquire");
            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(2));
            active.fetch_sub(1, Ordering::SeqCst);
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(peak.load(Ordering::SeqCst), 1);
}
