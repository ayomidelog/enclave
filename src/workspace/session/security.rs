use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use nix::mount::{mount, umount2, MntFlags, MsFlags};

const CAP_CHOWN: u32 = 0;
const CAP_DAC_OVERRIDE: u32 = 1;
const CAP_FOWNER: u32 = 3;
const CAP_KILL: u32 = 5;
const CAP_SETGID: u32 = 6;
const CAP_SETUID: u32 = 7;
const CAP_NET_BIND_SERVICE: u32 = 10;

const CAP_HEADER_VERSION_3: u32 = 0x2008_0522;
const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const MASK_ROOT: &str = "/run/enclave/masked";
const MASK_FILE_TARGETS: &[&str] = &[
    "/proc/kallsyms",
    "/proc/kcore",
    "/proc/keys",
    "/proc/modules",
    "/proc/sched_debug",
    "/proc/timer_list",
];
const MASK_DIR_TARGETS: &[&str] = &["/sys/kernel/debug", "/sys/kernel/security", "/sys/module"];
const EXEC_CAPABILITIES: &[u32] = &[
    CAP_CHOWN,
    CAP_DAC_OVERRIDE,
    CAP_FOWNER,
    CAP_KILL,
    CAP_SETGID,
    CAP_SETUID,
    CAP_NET_BIND_SERVICE,
];

#[repr(C)]
struct CapUserHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CapUserData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

pub fn apply_exec_restrictions() -> Result<()> {
    apply_capability_policy(EXEC_CAPABILITIES)?;
    set_no_new_privs()?;
    install_seccomp_filter(true)
}

pub fn apply_session_restrictions() -> Result<()> {
    apply_capability_policy(&[])?;
    set_no_new_privs()?;
    install_seccomp_filter(false)
}

pub fn tighten_namespace_mounts() -> Result<()> {
    if Path::new("/proc/sys").exists() {
        remount_read_only_with_policy(Path::new("/proc/sys"))?;
    }
    if Path::new("/sys").exists() {
        remount_read_only_with_policy(Path::new("/sys"))?;
    }
    if Path::new("/sys/fs/cgroup").exists() {
        remount_read_only_with_policy(Path::new("/sys/fs/cgroup"))?;
    }
    Ok(())
}

pub fn mask_runtime_paths() -> Result<()> {
    let file_root = Path::new(MASK_ROOT).join("files");
    let dir_root = Path::new(MASK_ROOT).join("dirs");
    fs::create_dir_all(&file_root)
        .with_context(|| format!("failed to create {}", file_root.display()))?;
    fs::create_dir_all(&dir_root)
        .with_context(|| format!("failed to create {}", dir_root.display()))?;

    for target in MASK_FILE_TARGETS {
        let target = Path::new(target);
        if !target.exists() {
            continue;
        }
        let source = file_root.join(mask_name_for_path(target));
        fs::write(&source, b"").with_context(|| format!("failed to write {}", source.display()))?;
        bind_mask(&source, target)?;
    }

    for target in MASK_DIR_TARGETS {
        let target = Path::new(target);
        if !target.exists() {
            continue;
        }
        let source = dir_root.join(mask_name_for_path(target));
        fs::create_dir_all(&source)
            .with_context(|| format!("failed to create {}", source.display()))?;
        bind_mask(&source, target)?;
    }

    Ok(())
}

pub fn detach_old_root(old_root: &Path) -> Result<()> {
    if !old_root.exists() {
        return Ok(());
    }
    umount2(old_root, MntFlags::MNT_DETACH)
        .with_context(|| format!("failed to detach old root {}", old_root.display()))?;
    if let Err(err) = fs::remove_dir(old_root) {
        if err.kind() != std::io::ErrorKind::NotFound {
            return Err(err)
                .with_context(|| format!("failed to remove old root {}", old_root.display()));
        }
    }
    Ok(())
}

fn bind_remount_read_only(path: &Path) -> Result<()> {
    mount(
        Some(path),
        path,
        Option::<&str>::None,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        Option::<&str>::None,
    )
    .with_context(|| format!("failed to bind-mount {}", path.display()))?;
    mount(
        Option::<&str>::None,
        path,
        Option::<&str>::None,
        MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY | MsFlags::MS_REC,
        Option::<&str>::None,
    )
    .with_context(|| format!("failed to remount {} read-only", path.display()))?;
    Ok(())
}

fn remount_read_only_with_policy(path: &Path) -> Result<()> {
    if let Err(err) = bind_remount_read_only(path) {
        if should_ignore_readonly_remount_error(path, &err) {
            tracing::warn!(
                "workspace runtime could not remount {} read-only; continuing with remaining hardening: {err:#}",
                path.display()
            );
            return Ok(());
        }
        return Err(err).with_context(|| format!("failed to remount {} read-only", path.display()));
    }
    Ok(())
}

fn should_ignore_readonly_remount_error(path: &Path, err: &anyhow::Error) -> bool {
    matches!(path.to_str(), Some("/sys") | Some("/sys/fs/cgroup"))
        && error_has_errno(err, libc::EPERM)
}

fn error_has_errno(err: &anyhow::Error, errno: i32) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .and_then(|io_err| io_err.raw_os_error())
            == Some(errno)
            || cause
                .downcast_ref::<nix::errno::Errno>()
                .map(|nix_err| *nix_err as i32)
                == Some(errno)
    })
}

fn bind_mask(source: &Path, target: &Path) -> Result<()> {
    mount(
        Some(source),
        target,
        Option::<&str>::None,
        MsFlags::MS_BIND,
        Option::<&str>::None,
    )
    .with_context(|| {
        format!(
            "failed to bind mask {} over {}",
            source.display(),
            target.display()
        )
    })?;
    mount(
        Option::<&str>::None,
        target,
        Option::<&str>::None,
        MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY,
        Option::<&str>::None,
    )
    .with_context(|| {
        format!(
            "failed to remount masked path {} read-only",
            target.display()
        )
    })?;
    Ok(())
}

fn apply_capability_policy(keep_caps: &[u32]) -> Result<()> {
    let keep: BTreeSet<u32> = keep_caps.iter().copied().collect();
    let last_cap = read_cap_last_cap().unwrap_or(40);
    for cap in 0..=last_cap {
        if keep.contains(&cap) {
            continue;
        }
        let rc = unsafe { libc::prctl(libc::PR_CAPBSET_DROP, cap as libc::c_ulong, 0, 0, 0) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("failed to drop capability {} from bounding set", cap));
        }
    }

    let mut data = [
        CapUserData {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
        CapUserData {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
    ];
    for cap in keep {
        let index = (cap / 32) as usize;
        let mask = 1u32 << (cap % 32);
        data[index].effective |= mask;
        data[index].permitted |= mask;
        data[index].inheritable |= mask;
    }
    let mut header = CapUserHeader {
        version: CAP_HEADER_VERSION_3,
        pid: 0,
    };
    let rc = unsafe {
        libc::syscall(
            libc::SYS_capset,
            &mut header as *mut CapUserHeader,
            data.as_mut_ptr(),
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("capset failed");
    }

    Ok(())
}

fn set_no_new_privs() -> Result<()> {
    let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to set no_new_privs");
    }
    Ok(())
}

fn install_seccomp_filter(allow_clone3: bool) -> Result<()> {
    let deny_action = SECCOMP_RET_ERRNO | libc::EPERM as u32;
    let mut filter = vec![
        stmt((libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16, 4),
        jump(
            (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            AUDIT_ARCH_X86_64,
            1,
            0,
        ),
        stmt(
            (libc::BPF_RET | libc::BPF_K) as u16,
            SECCOMP_RET_KILL_PROCESS,
        ),
        stmt((libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16, 0),
    ];

    for syscall in denied_syscalls(allow_clone3) {
        filter.push(jump(
            (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            syscall,
            0,
            1,
        ));
        filter.push(stmt((libc::BPF_RET | libc::BPF_K) as u16, deny_action));
    }

    filter.push(stmt(
        (libc::BPF_RET | libc::BPF_K) as u16,
        SECCOMP_RET_ALLOW,
    ));
    let prog = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_mut_ptr(),
    };

    let rc = unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &prog as *const libc::sock_fprog,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to install seccomp filter");
    }
    Ok(())
}

fn denied_syscalls(allow_clone3: bool) -> Vec<u32> {
    let mut syscalls = vec![
        libc::SYS_acct as u32,
        libc::SYS_add_key as u32,
        libc::SYS_bpf as u32,
        libc::SYS_delete_module as u32,
        libc::SYS_finit_module as u32,
        libc::SYS_fsconfig as u32,
        libc::SYS_fsopen as u32,
        libc::SYS_fsmount as u32,
        libc::SYS_init_module as u32,
        libc::SYS_io_uring_enter as u32,
        libc::SYS_io_uring_register as u32,
        libc::SYS_io_uring_setup as u32,
        libc::SYS_kcmp as u32,
        libc::SYS_kexec_file_load as u32,
        libc::SYS_kexec_load as u32,
        libc::SYS_keyctl as u32,
        libc::SYS_mknod as u32,
        libc::SYS_mknodat as u32,
        libc::SYS_mount as u32,
        libc::SYS_mount_setattr as u32,
        libc::SYS_move_mount as u32,
        libc::SYS_name_to_handle_at as u32,
        libc::SYS_open_by_handle_at as u32,
        libc::SYS_open_tree as u32,
        libc::SYS_perf_event_open as u32,
        libc::SYS_personality as u32,
        libc::SYS_pivot_root as u32,
        libc::SYS_process_vm_readv as u32,
        libc::SYS_process_vm_writev as u32,
        libc::SYS_ptrace as u32,
        libc::SYS_quotactl as u32,
        libc::SYS_reboot as u32,
        libc::SYS_request_key as u32,
        libc::SYS_setns as u32,
        libc::SYS_swapoff as u32,
        libc::SYS_swapon as u32,
        libc::SYS_syslog as u32,
        libc::SYS_umount2 as u32,
        libc::SYS_unshare as u32,
        libc::SYS_userfaultfd as u32,
    ];
    if !allow_clone3 {
        syscalls.push(libc::SYS_clone3 as u32);
    }
    syscalls
}

fn stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

fn read_cap_last_cap() -> Option<u32> {
    fs::read_to_string("/proc/sys/kernel/cap_last_cap")
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
}

fn mask_name_for_path(path: &Path) -> String {
    path.to_string_lossy()
        .trim_matches('/')
        .replace('/', "__")
        .replace('.', "_")
}

#[cfg(test)]
#[path = "../../../tests/src/workspace/session/security.rs"]
mod tests;
