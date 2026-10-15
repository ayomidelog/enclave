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
