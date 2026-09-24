use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, create_workspace_snapshot, destroy_workspace, exec_workspace_command,
    list_workspaces, resize_workspace_disk, restore_workspace_snapshot, start_workspace,
    stop_workspace, workspace_runtime_info, WorkspaceLimits,
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

struct SandboxCleanup {
    state_dir: PathBuf,
    sandbox_id: Option<String>,
}

impl SandboxCleanup {
    fn new(state_dir: PathBuf) -> Self {
        Self {
            state_dir,
            sandbox_id: None,
        }
    }

    fn record(&mut self, sandbox_id: &str) {
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

fn prepare_cached_rootfs(state_dir: &Path, suite: &str) {
    let cache = state_dir.join("sandboxes").join("rootfs-cache").join(suite);
    fs::create_dir_all(cache.join("bin")).expect("create bin");
    fs::create_dir_all(cache.join("etc")).expect("create etc");
    fs::create_dir_all(cache.join("opt")).expect("create opt");
    fs::create_dir_all(cache.join("usr").join("bin")).expect("create usr/bin");
    fs::copy("/usr/bin/busybox", cache.join("bin").join("busybox")).expect("copy busybox");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/bin/busybox", cache.join("bin").join("sh"))
            .expect("symlink sh");
        std::os::unix::fs::symlink("/bin/busybox", cache.join("bin").join("dd"))
            .expect("symlink dd");
        std::os::unix::fs::symlink("/bin/busybox", cache.join("bin").join("cat"))
            .expect("symlink cat");
        std::os::unix::fs::symlink("/bin/busybox", cache.join("usr").join("bin").join("env"))
            .expect("symlink env");
    }
}

/// Whether a persistent workspace session helper is still running for `socket`.
fn persistent_helper_is_running(socket: &str) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // A zombie has already released its namespaces and cgroup membership.
        let state = stat
            .rsplit_once(") ")
            .and_then(|(_, rest)| rest.chars().next());
        if matches!(state, Some('Z') | Some('X')) {
            continue;
        }
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&cmdline);
        if cmdline.contains("workspace-session-persistent-helper") && cmdline.contains(socket) {
            return true;
        }
    }
    false
}

#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn workspace_root_overlay_enforces_total_disk_quota() {
    if !root_only() {
        return;
    }

    use std::io::Write;

    let state = state_dir("enclave-int-root-overlay-quota");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-root-overlay-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let quota_bytes = 32 * 1024 * 1024;
    let limited = create_workspace(
        &state,
        &sandbox.id,
        "limited",
        WorkspaceLimits {
            disk_bytes: Some(quota_bytes),
            ..WorkspaceLimits::default()
        },
    )
    .expect("create quota workspace");
    let peer = create_workspace(&state, &sandbox.id, "peer", WorkspaceLimits::default())
        .expect("create peer workspace");
    let limited = start_workspace(&state, &sandbox.id, &limited.id).expect("start quota workspace");
    let peer = start_workspace(&state, &sandbox.id, &peer.id).expect("start peer workspace");
    let limited_pid = limited.runtime_pid.expect("quota runtime pid");
    let peer_pid = peer.runtime_pid.expect("peer runtime pid");

    let limited_fill = Path::new("/proc")
        .join(limited_pid.to_string())
        .join("root/opt/quota-fill.bin");
    let peer_fill = Path::new("/proc")
        .join(peer_pid.to_string())
        .join("root/opt/quota-fill.bin");
    let lower_fill = Path::new(&sandbox.rootfs_path).join("opt/quota-fill.bin");
    let block = vec![0u8; 1024 * 1024];
    let mut file = fs::File::create(&limited_fill).expect("create quota fill file");
    let mut wrote_any = false;
    let mut hit_quota = false;
    for _ in 0..64 {
        match file.write_all(&block) {
            Ok(()) => wrote_any = true,
            Err(error) => {
                assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
                hit_quota = true;
                break;
            }
        }
    }
    assert!(wrote_any, "quota overlay should accept some writes");
    assert!(
        hit_quota,
        "quota overlay should reject writes after exhaustion"
    );
    assert!(
        !peer_fill.exists(),
        "peer workspace must not see quota workspace writes"
    );
    assert!(
        !lower_fill.exists(),
        "shared sandbox lower rootfs must remain unchanged"
    );

    stop_workspace(&state, &sandbox.id, &limited.id).expect("stop quota workspace");
    stop_workspace(&state, &sandbox.id, &peer.id).expect("stop peer workspace");
    destroy_workspace(&state, &sandbox.id, &limited.id).expect("destroy quota workspace");
    destroy_workspace(&state, &sandbox.id, &peer.id).expect("destroy peer workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn snapshot_restore_recovers_filesystem_state() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-snapshot");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-snapshot-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let workspace = create_workspace(&state, &sandbox.id, "ws", WorkspaceLimits::default())
        .expect("create workspace");

    fs::write(
        Path::new(&workspace.filesystem_path).join("state.txt"),
        "before",
    )
    .expect("seed file");
    create_workspace_snapshot(&state, &sandbox.id, &workspace.id, Some("snap1"))
        .expect("create snapshot");

    fs::write(
        Path::new(&workspace.filesystem_path).join("state.txt"),
        "after",
    )
    .expect("mutate");
    restore_workspace_snapshot(&state, &sandbox.id, &workspace.id, "snap1").expect("restore");

    let restored = fs::read_to_string(Path::new(&workspace.filesystem_path).join("state.txt"))
        .expect("read restored file");
    assert_eq!(restored, "before");

    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

#[test]
#[ignore = "requires root privileges and namespace/cgroup support"]
fn cgroup_limits_are_applied_to_workspace_runtime() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-cgroup");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-cgroup-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let limits = WorkspaceLimits {
        cpu_percent: Some(10.0),
        memory_bytes: Some(128 * 1024 * 1024),
        max_processes: Some(64),
        ..WorkspaceLimits::default()
    };
    let workspace =
        create_workspace(&state, &sandbox.id, "limits", limits).expect("create workspace");
    start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");

    let runtime = workspace_runtime_info(&state, &sandbox.id, &workspace.id).expect("runtime info");
    assert!(runtime.runtime_pid > 0);

    let cgroup_path = runtime
        .cgroup_path
        .as_deref()
        .expect("a limited workspace must report its cgroup");
    assert!(
        Path::new(cgroup_path).is_dir(),
        "{cgroup_path} is not a cgroup directory"
    );
    let cpu_max = fs::read_to_string(Path::new(cgroup_path).join("cpu.max")).expect("read cpu.max");
    assert_ne!(
        cpu_max.split_whitespace().next().unwrap_or_default(),
        "max",
        "cpu_percent must cap the workspace cgroup: {cpu_max}"
    );

    // The daemon-managed exec path runs commands through a persistent helper;
    // the commands it forks must inherit the workspace cgroup, otherwise the
    // declared limits would not apply to them.
    let result = exec_workspace_command(
        &state,
        &sandbox.id,
        &workspace.id,
        "/home",
        &[
            "sh".to_string(),
            "-c".to_string(),
            "cat /proc/self/cgroup".to_string(),
        ],
    )
    .expect("run workspace command");
    assert_eq!(result.exit_code, 0, "stderr: {}", result.stderr);
    let expected = format!("enclave-ws-{}-{}", sandbox.id, workspace.id);
    assert!(
        result.stdout.contains(&expected),
        "expected the command to run inside {expected}, got: {}",
        result.stdout
    );

    // A helper that outlives its runtime would hold the workspace cgroup and
    // its mount namespace forever, so it has to notice the runtime's death and
    // exit on its own.
    let helper_socket = format!(
        "/run/enclave/session-{}-{}.sock",
        runtime.runtime_pid, runtime.runtime_starttime_ticks
    );
    assert!(
        persistent_helper_is_running(&helper_socket),
        "expected a live helper for {helper_socket}"
    );
    unsafe { libc::kill(runtime.runtime_pid as i32, libc::SIGKILL) };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut exited = false;
    while std::time::Instant::now() < deadline {
        if !persistent_helper_is_running(&helper_socket) {
            exited = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(
        exited,
        "the persistent helper must exit once its runtime is gone"
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

#[test]
#[ignore = "requires root privileges, namespace/mount support, and loopback ext4 mounts"]
fn workspace_disk_quota_caps_enclave_managed_home_storage() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-disk-quota");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-disk-quota-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let limits = WorkspaceLimits {
        disk_bytes: Some(64 * 1024 * 1024),
        ..WorkspaceLimits::default()
    };
    let workspace =
        create_workspace(&state, &sandbox.id, "quota", limits).expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let runtime_pid = started.runtime_pid.expect("runtime pid");
    let quota_home = Path::new("/proc")
        .join(runtime_pid.to_string())
        .join("root/home");
    let large_file = quota_home.join("fill.bin");
    let payload = vec![b'a'; 1024 * 1024];
    let mut file = std::fs::File::create(&large_file).expect("create fill file");
    let mut wrote_any = false;
    let mut hit_quota = false;
    for _ in 0..80 {
        use std::io::Write;
        match file.write_all(&payload) {
            Ok(()) => wrote_any = true,
            Err(err) => {
                hit_quota = true;
                assert_eq!(
                    err.raw_os_error(),
                    Some(libc::ENOSPC),
                    "expected ENOSPC once workspace quota is exhausted, got: {err}"
                );
                break;
            }
        }
    }
    assert!(
        wrote_any,
        "quota-backed workspace should accept some writes before exhaustion"
    );
    assert!(
        hit_quota,
        "quota-backed workspace should eventually reject writes with ENOSPC"
    );
    assert!(
        large_file.exists(),
        "partial file should exist after quota exhaustion attempt"
    );
    let size = fs::metadata(&large_file).expect("fill file metadata").len();
    assert!(
        size < 80 * 1024 * 1024,
        "quota-backed workspace should stop short of requested size; got {} bytes",
        size
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

#[test]
#[ignore = "requires root privileges, namespace/mount support, and loopback ext4 mounts"]
fn workspace_disk_backed_tmp_is_linked_writable_and_uses_workspace_storage() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-disk-backed-tmp");
    let mut cleanup = SandboxCleanup::new(state.clone());
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-disk-backed-tmp-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    cleanup.record(&sandbox.id);
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let workspace = create_workspace(
        &state,
        &sandbox.id,
        "tmp",
        WorkspaceLimits {
            disk_bytes: Some(64 * 1024 * 1024),
            ..WorkspaceLimits::default()
        },
    )
    .expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let runtime_pid = started.runtime_pid.expect("runtime pid");
    let runtime_root = Path::new("/proc")
        .join(runtime_pid.to_string())
        .join("root");
    let runtime_tmp = runtime_root.join("tmp");
    let runtime_backing = runtime_root.join("home").join(".enclave-tmp");
    let runtime_home_tmp = runtime_root.join("home/tmp");

    let tmp_metadata = fs::metadata(&runtime_tmp).expect("workspace /tmp metadata");
    let backing_metadata =
        fs::metadata(&runtime_backing).expect("workspace /tmp backing directory metadata");
    assert!(tmp_metadata.is_dir());
    assert!(
        tmp_metadata.nlink() >= 2,
        "workspace /tmp must have a live directory link"
    );
    assert_eq!(tmp_metadata.dev(), backing_metadata.dev());
    assert_eq!(tmp_metadata.ino(), backing_metadata.ino());
    assert_eq!(tmp_metadata.permissions().mode() & 0o1777, 0o1777);
    // The backing directory must not be reachable as `/home/tmp`. When it was,
    // removing that entry unlinked the live `/tmp` mount and every later write
    // under `/tmp` failed with ENOENT.
    assert!(
        !runtime_home_tmp.exists(),
        "the /tmp backing directory must not be aliased in the workspace home view"
    );

    let churn = exec_workspace_command(
        &state,
        &sandbox.id,
        &workspace.id,
        "/home",
        &[
            "sh".to_string(),
            "-c".to_string(),
            "mkdir -p /home/tmp && rm -rf /home/tmp && printf ok > /tmp/enclave-alias-probe && rm /tmp/enclave-alias-probe".to_string(),
        ],
    )
    .expect("churn /home/tmp");
    assert_eq!(
        churn.exit_code, 0,
        "creating and removing /home/tmp must not affect /tmp: {}",
        churn.stderr
    );
    let after_churn = fs::metadata(&runtime_tmp).expect("workspace /tmp metadata after churn");
    assert_eq!(after_churn.ino(), tmp_metadata.ino());
    assert!(after_churn.nlink() >= 2);

    let result = exec_workspace_command(
        &state,
        &sandbox.id,
        &workspace.id,
        "/home",
        &[
            "sh".to_string(),
            "-c".to_string(),
            "test \"$TMPDIR\" = /tmp && printf ok > /tmp/enclave-tmp-probe && test \"$(cat /tmp/enclave-tmp-probe)\" = ok && rm /tmp/enclave-tmp-probe".to_string(),
        ],
    )
    .expect("execute /tmp write probe");
    assert_eq!(
        result.exit_code, 0,
        "workspace command failed: {}",
        result.stderr
    );
}

/// Size of the ext2/3/4 filesystem recorded in a disk image's superblock.
///
/// Resizing an image can grow the file while leaving the filesystem inside it
/// at the previous size, so tests assert the filesystem itself reached the
/// requested allocation instead of trusting the image file size.
fn ext4_filesystem_size(image: &Path) -> std::io::Result<u64> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = fs::File::open(image)?;
    let mut superblock = [0u8; 1024];
    file.seek(SeekFrom::Start(1024))?;
    file.read_exact(&mut superblock)?;
    let magic = u16::from_le_bytes([superblock[0x38], superblock[0x39]]);
    if magic != 0xEF53 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not an ext2/3/4 filesystem", image.display()),
        ));
    }
    let blocks_low = u64::from(u32::from_le_bytes(
        superblock[0x04..0x08].try_into().expect("4 bytes"),
    ));
    let log_block_size = u32::from_le_bytes(superblock[0x18..0x1c].try_into().expect("4 bytes"));
    let block_size = 1024u64 << log_block_size;
    let incompat = u32::from_le_bytes(superblock[0x60..0x64].try_into().expect("4 bytes"));
    let blocks = if incompat & 0x80 != 0 {
        blocks_low
            | (u64::from(u32::from_le_bytes(
                superblock[0x150..0x154].try_into().expect("4 bytes"),
            )) << 32)
    } else {
        blocks_low
    };
    Ok(blocks * block_size)
}

#[test]
#[ignore = "requires root privileges, namespace/mount support, and loopback ext4 mounts"]
fn workspace_disk_resize_grows_running_managed_storage() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-disk-resize");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-disk-resize-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let initial_bytes = 64 * 1024 * 1024;
    let expanded_bytes = 96 * 1024 * 1024;
    let workspace = create_workspace(
        &state,
        &sandbox.id,
        "resize",
        WorkspaceLimits {
            disk_bytes: Some(initial_bytes),
            ..WorkspaceLimits::default()
        },
    )
    .expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let old_pid = started.runtime_pid.expect("runtime pid");
    let workspace_file = Path::new("/proc")
        .join(old_pid.to_string())
        .join("root/home/preserved.txt");
    fs::write(&workspace_file, "preserve me").expect("write workspace data");

    let result = resize_workspace_disk(&state, &sandbox.id, &workspace.id, expanded_bytes)
        .expect("resize workspace");
    assert!(result.restarted);
    assert_eq!(result.previous_disk_bytes, initial_bytes);
    assert_eq!(result.new_disk_bytes, expanded_bytes);

    let workspaces = list_workspaces(&state, Some(&sandbox.id)).expect("list workspaces");
    let resized = workspaces
        .into_iter()
        .find(|item| item.id == workspace.id)
        .expect("resized workspace metadata");
    assert_eq!(resized.limits.disk_bytes, Some(expanded_bytes));
    let new_pid = resized.runtime_pid.expect("restarted runtime pid");
    assert_ne!(new_pid, old_pid);
    assert_eq!(
        fs::read_to_string(
            Path::new("/proc")
                .join(new_pid.to_string())
                .join("root/home/preserved.txt")
        )
        .expect("read preserved workspace data"),
        "preserve me"
    );
    assert_eq!(
        fs::metadata(Path::new(&workspace.workspace_path).join("fs.img"))
            .expect("disk image metadata")
            .len(),
        expanded_bytes
    );
    // The image file and the ext4 filesystem inside it must agree. A resize that
    // grows only the image leaves the workspace failing its readiness checks.
    let filesystem_bytes =
        ext4_filesystem_size(&Path::new(&workspace.workspace_path).join("fs.img"))
            .expect("read ext4 filesystem size");
    assert!(
        filesystem_bytes >= expanded_bytes,
        "ext4 filesystem is {filesystem_bytes} bytes, below the requested {expanded_bytes}"
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    let stopped_result = resize_workspace_disk(&state, &sandbox.id, &workspace.id, expanded_bytes)
        .expect("equal stopped resize");
    assert!(!stopped_result.restarted);
    assert!(list_workspaces(&state, Some(&sandbox.id))
        .expect("list stopped workspace")
        .into_iter()
        .find(|item| item.id == workspace.id)
        .expect("stopped workspace metadata")
        .runtime_pid
        .is_none());

    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

#[test]
#[ignore = "requires root privileges, namespace/mount support, and loopback ext4 mounts"]
fn snapshot_restore_recovers_quota_backed_workspace_state() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-snapshot-disk-quota");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-snapshot-quota-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let limits = WorkspaceLimits {
        disk_bytes: Some(64 * 1024 * 1024),
        ..WorkspaceLimits::default()
    };
    let workspace =
        create_workspace(&state, &sandbox.id, "quota-snap", limits).expect("create workspace");

    fs::write(
        Path::new(&workspace.filesystem_path).join("state.txt"),
        "before",
    )
    .expect("seed file");
    create_workspace_snapshot(&state, &sandbox.id, &workspace.id, Some("snap1"))
        .expect("create snapshot");

    fs::write(
        Path::new(&workspace.filesystem_path).join("state.txt"),
        "after",
    )
    .expect("mutate");
    restore_workspace_snapshot(&state, &sandbox.id, &workspace.id, "snap1").expect("restore");

    start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let restored = fs::read_to_string(Path::new(&workspace.filesystem_path).join("state.txt"))
        .expect("read restored file");
    assert_eq!(restored, "before");

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
