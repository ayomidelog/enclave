//! The capability and seccomp policy a workspace session runs under.
//!
//! Both an exec and a session start by dropping every capability the workspace
//! does not need from the bounding set, and then installing a seccomp filter that
//! returns `EPERM` for the syscalls which would let a process reach back into the
//! host: mounting, namespace changes, kernel module and key management, tracing
//! another process, and the newer filesystem and io_uring entry points.
//!
//! An exec keeps a small set of capabilities because a user command such as a
//! package manager legitimately needs to change ownership and to bind a low port.
//! A session keeps none.

use std::collections::BTreeSet;
use std::fs;

use anyhow::{Context, Result};

pub(super) const CAP_CHOWN: u32 = 0;
const CAP_DAC_OVERRIDE: u32 = 1;
const CAP_FOWNER: u32 = 3;
const CAP_KILL: u32 = 5;
pub(super) const CAP_SETGID: u32 = 6;
pub(super) const CAP_SETUID: u32 = 7;
const CAP_NET_BIND_SERVICE: u32 = 10;

const CAP_HEADER_VERSION_3: u32 = 0x2008_0522;
const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;

pub(super) const EXEC_CAPABILITIES: &[u32] = &[
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

/// The policy a workspace exec runs under: a small capability set, no new
/// privileges, and a filter that leaves `clone3` available because the glibc and
/// language runtimes a user command may start still call it.
pub(crate) fn apply_exec_restrictions() -> Result<()> {
    apply_capability_policy(EXEC_CAPABILITIES)?;
    set_no_new_privs()?;
    install_seccomp_filter(true)
}

/// The policy a workspace session runs under: no capabilities at all, and a
/// filter that also denies `clone3`.
pub(crate) fn apply_session_restrictions() -> Result<()> {
    apply_capability_policy(&[])?;
    set_no_new_privs()?;
    install_seccomp_filter(false)
}

/// Drop everything outside `keep_caps` from the bounding set, then reduce the
/// effective, permitted, and inheritable sets to exactly `keep_caps`.
///
/// The bounding set is the ceiling for what a process may ever acquire, so
/// dropping from it first means the `capset` that follows cannot be undone by a
/// later exec.
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

    // `capset` takes the two 32-bit words that cover capabilities 0 through 63.
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

/// Install the seccomp filter.
///
/// The first instruction loads the audit architecture and kills the process
/// outright if it does not match `x86_64`: a filter built for one architecture
/// must never be applied to a process running under another, because the syscall
/// numbers would mean different calls. The remaining instructions compare the
/// syscall number against the denied list, return `EPERM` for a match, and allow
/// everything else.
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

/// The syscalls the filter returns `EPERM` for.
///
/// Every entry is a way to reach outside the workspace: mount and namespace
/// manipulation, kernel modules, keys, tracing another process, the block layer,
/// the newer mount API, io_uring, and the syslog. `clone3` is excluded from an
/// exec because a user command may still start a runtime that uses it.
pub(super) fn denied_syscalls(allow_clone3: bool) -> Vec<u32> {
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

/// The highest capability number this kernel defines, so the bounding set is
/// cleared up to the real ceiling rather than a hard-coded guess. A kernel that
/// does not expose the file falls back to 40, the value for the kernels Enclave
/// supports.
fn read_cap_last_cap() -> Option<u32> {
    fs::read_to_string("/proc/sys/kernel/cap_last_cap")
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
}
