//! Removing a workspace's cgroup, including the moment it is still draining.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::Duration;

use enclave::sandbox::cgroup::{is_cgroup_v2_available, remove_cgroup_path};

fn root_only() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// A cgroup of this test's own, so nothing else on the host is touched.
struct ScratchCgroup {
    path: PathBuf,
}

impl ScratchCgroup {
    fn new(label: &str) -> Option<Self> {
        if !root_only() || !is_cgroup_v2_available() {
            return None;
        }
        let path = Path::new("/sys/fs/cgroup")
            .join(format!("enclave-test-{label}-{}", std::process::id()));
        let _ = fs::remove_dir(&path);
        fs::create_dir(&path).expect("create the scratch cgroup");
        Some(Self { path })
    }

    /// Put `child` in this cgroup, which is what makes the cgroup non-removable.
    fn adopt(&self, child: &Child) {
        fs::write(self.path.join("cgroup.procs"), child.id().to_string())
            .expect("add the process to the scratch cgroup");
        // The write returns before the process is necessarily listed, and the
        // removal below is only meaningful once it is.
        for _ in 0..100 {
            let listed = fs::read_to_string(self.path.join("cgroup.procs"))
                .map(|raw| {
                    raw.lines()
                        .any(|line| line.trim() == child.id().to_string())
                })
                .unwrap_or(false);
            if listed {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!(
            "process {} never appeared in {}",
            child.id(),
            self.path.display()
        );
    }
}

impl Drop for ScratchCgroup {
    fn drop(&mut self) {
        // The test owns this cgroup, so it also owns whatever is left in it. A
        // failed assertion must not leave a cgroup behind on a shared host, so
        // the process is signalled and the removal is retried until the kernel
        // lets go rather than attempted once.
        for _ in 0..40 {
            if fs::remove_dir(&self.path).is_ok() {
                return;
            }
            if let Ok(raw) = fs::read_to_string(self.path.join("cgroup.procs")) {
                for pid in raw
                    .lines()
                    .filter_map(|line| line.trim().parse::<i32>().ok())
                {
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
            thread::sleep(Duration::from_millis(25));
        }
        let _ = fs::remove_dir(&self.path);
    }
}

/// A cgroup whose last process is still leaving must be removed, not reported as
/// a cleanup failure.
///
/// The kernel refuses to remove a cgroup while a process is listed in it, and it
/// says so with `EBUSY`. That refusal is transient in the case that matters: a
/// command a workspace ran is reaped when the runtime that owned its pid namespace
/// exits, and the helper that launched it is itself in the workspace cgroup and
/// only exits once its own wait returns. Retrying only the `ENOTEMPTY` answer left
/// that moment as a reported failure on a stop that had released everything.
#[test]
#[ignore = "requires root privileges and cgroup v2"]
fn removing_a_cgroup_waits_for_a_process_that_is_leaving() {
    let Some(cgroup) = ScratchCgroup::new("draining") else {
        return;
    };
    let mut child = Command::new("sleep")
        .arg("0.05")
        .spawn()
        .expect("spawn a process that leaves on its own");
    cgroup.adopt(&child);

    remove_cgroup_path(&cgroup.path)
        .expect("removal must wait for a process that is on its way out");
    assert!(
        !cgroup.path.exists(),
        "removal reported success but {} is still there",
        cgroup.path.display()
    );

    let _ = child.wait();
}

/// A cgroup whose process is staying must fail, and must fail within its budget.
///
/// The retry above is only safe because it is bounded. A stop that cannot release
/// a cgroup has to say so rather than wait forever or claim success, and the error
/// has to name what is still holding it.
#[test]
#[ignore = "requires root privileges and cgroup v2"]
fn removing_a_cgroup_with_a_process_that_stays_fails_within_its_budget() {
    let Some(cgroup) = ScratchCgroup::new("staying") else {
        return;
    };
    let mut child = Command::new("sleep")
        .arg("300")
        .spawn()
        .expect("spawn a process that stays");
    cgroup.adopt(&child);

    let started = std::time::Instant::now();
    let error = remove_cgroup_path(&cgroup.path)
        .expect_err("removal must not report success while a process is in the cgroup");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "the retry budget must be bounded; removal took {elapsed:?}"
    );
    assert!(
        format!("{error:#}").contains("still busy"),
        "the error must name the state that held the cgroup: {error:#}"
    );

    // The cgroup is this test's own, so releasing it is too.
    let _ = child.kill();
    let _ = child.wait();
    for _ in 0..100 {
        if fs::remove_dir(&cgroup.path).is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
}
