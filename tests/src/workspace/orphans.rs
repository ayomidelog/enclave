use super::*;

fn fixture_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "enclave-orphan-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("ns")).expect("create ns dir");
    fs::create_dir_all(dir.join("runtime")).expect("create runtime dir");
    dir
}

#[test]
fn a_directory_with_no_markers_and_no_cgroup_has_no_owner() {
    let dir = fixture_dir("empty");
    assert_eq!(find_orphan_runtime(&dir, "sb-absent", "ws-absent"), None);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_stopped_workspace_marker_is_not_an_owner() {
    // A stop writes `unassigned` into the reference files, so the marker is still
    // there after the runtime is gone. It must not read as a live owner.
    let dir = fixture_dir("unassigned");
    fs::write(dir.join("ns/pid.ref"), "unassigned\n").expect("write marker");
    fs::write(dir.join("ns/mnt.ref"), "unassigned\n").expect("write marker");
    assert_eq!(find_orphan_runtime(&dir, "sb-absent", "ws-absent"), None);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_marker_for_a_live_namespace_names_the_owning_process() {
    // The current process's own pid namespace is a real, live namespace, so a
    // marker naming it must resolve back to this process. That is the discovery
    // path used when `workspace.json` is gone: the namespace inode is what proves
    // ownership, not a pid that may have been reused.
    let dir = fixture_dir("live");
    let namespace = fs::read_link("/proc/self/ns/pid").expect("read own pid namespace");
    fs::write(
        dir.join("ns/pid.ref"),
        format!("{}\n", namespace.to_string_lossy()),
    )
    .expect("write marker");

    let orphan = find_orphan_runtime(&dir, "sb-absent", "ws-absent")
        .expect("a live namespace must be discovered");
    let discovered = orphan.runtime_pid.expect("a pid in the namespace");
    assert_eq!(
        fs::read_link(format!("/proc/{discovered}/ns/pid"))
            .expect("read the discovered process namespace")
            .to_string_lossy(),
        namespace.to_string_lossy()
    );
    assert_eq!(
        orphan.pid_namespace.as_deref(),
        Some(namespace.to_string_lossy().as_ref())
    );
    assert!(orphan.describe().contains("pid namespace") || orphan.describe().contains("pid "));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_marker_for_a_dead_namespace_finds_no_process() {
    let dir = fixture_dir("dead");
    // A namespace inode that cannot exist, so nothing in /proc can match it.
    fs::write(dir.join("ns/pid.ref"), "pid:[1]\n").expect("write marker");
    assert_eq!(find_orphan_runtime(&dir, "sb-absent", "ws-absent"), None);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_live_session_pid_file_is_an_owner_when_the_marker_is_gone() {
    // The pid file survives a crash and names the session directly, so it is the
    // fallback when the namespace reference was never written.
    let dir = fixture_dir("pidfile");
    fs::write(
        dir.join("runtime/session.pid"),
        format!("{}\n", std::process::id()),
    )
    .expect("write pid file");
    let orphan = find_orphan_runtime(&dir, "sb-absent", "ws-absent")
        .expect("a live pid file must be discovered");
    assert_eq!(orphan.runtime_pid, Some(std::process::id()));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_stale_session_pid_file_is_not_an_owner() {
    let dir = fixture_dir("stalepid");
    fs::write(dir.join("runtime/session.pid"), format!("{}\n", u32::MAX)).expect("write pid file");
    assert_eq!(find_orphan_runtime(&dir, "sb-absent", "ws-absent"), None);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_orphan_description_names_the_directory_owner() {
    let orphan = OrphanRuntime {
        runtime_pid: Some(4242),
        pid_namespace: Some("pid:[4026532812]".to_string()),
        cgroup: Some(PathBuf::from("/sys/fs/cgroup/enclave-sb-x/enclave-ws-x-y")),
    };
    let described = orphan.describe();
    assert!(described.contains("4242"), "{described}");
    assert!(described.contains("pid:[4026532812]"), "{described}");
    assert!(described.contains("enclave-ws-x-y"), "{described}");
}
