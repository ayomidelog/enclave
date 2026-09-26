use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[path = "../../src/network/dns.rs"]
mod dns_impl;

fn temp_rootfs(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("enclave-test-{}-{}", name, std::process::id()));
    if let Err(err) = fs::remove_dir_all(&dir) {
        if err.kind() != std::io::ErrorKind::NotFound {
            panic!("failed to clean temp rootfs {}: {err}", dir.display());
        }
    }
    fs::create_dir_all(&dir).expect("create temp rootfs");
    dir
}

#[test]
fn provision_resolv_conf_writes_managed_file() {
    let rootfs = temp_rootfs("dns-provision");
    dns_impl::provision_resolv_conf(&rootfs).expect("provision resolv.conf");

    let resolv = fs::read_to_string(rootfs.join("etc").join("resolv.conf"))
        .expect("read provisioned resolv.conf");
    assert!(
        resolv.starts_with("# Provisioned by Enclave"),
        "resolv.conf should include managed header"
    );
    assert!(!resolv.trim().is_empty(), "resolv.conf should not be empty");
}

#[test]
fn provision_etc_hosts_creates_missing_hosts_file() {
    let rootfs = temp_rootfs("hosts-provision");
    dns_impl::provision_etc_hosts(&rootfs).expect("provision /etc/hosts");

    let hosts =
        fs::read_to_string(rootfs.join("etc").join("hosts")).expect("read provisioned /etc/hosts");
    assert!(hosts.contains("127.0.0.1 localhost"));
}

#[test]
fn provision_etc_hosts_overwrites_existing_hosts_file() {
    let rootfs = temp_rootfs("hosts-overwrite");
    let hosts_path = rootfs.join("etc").join("hosts");
    fs::create_dir_all(hosts_path.parent().expect("hosts parent")).expect("create etc dir");
    fs::write(&hosts_path, "custom-host-entry\n").expect("write custom hosts");

    dns_impl::provision_etc_hosts(&rootfs).expect("provision /etc/hosts");

    let hosts = fs::read_to_string(&hosts_path).expect("read hosts");
    assert!(hosts.starts_with("# Provisioned by Enclave"));
    assert!(hosts.contains("127.0.0.1 localhost"));
}

#[test]
fn provision_apt_sandbox_override_writes_override_file() {
    let rootfs = temp_rootfs("apt-sandbox");

    fs::create_dir_all(rootfs.join("etc").join("apt")).expect("create /etc/apt");

    dns_impl::provision_apt_sandbox_override(&rootfs).expect("provision apt override");

    let conf = fs::read_to_string(
        rootfs
            .join("etc")
            .join("apt")
            .join("apt.conf.d")
            .join("99enclave-nosandbox-user"),
    )
    .expect("read apt override");
    assert!(conf.contains("APT::Sandbox::User \"root\";"));
}

#[test]
fn provision_apt_sandbox_override_skips_non_apt_rootfs() {
    let rootfs = temp_rootfs("apt-sandbox-skip");

    dns_impl::provision_apt_sandbox_override(&rootfs)
        .expect("provision should succeed for non-APT rootfs");

    assert!(
        !rootfs.join("etc").join("apt").exists(),
        "/etc/apt must not be created for non-APT rootfs"
    );
}

#[test]
fn is_apt_based_rootfs_detects_etc_apt_dir() {
    let rootfs = temp_rootfs("apt-detect-etc");
    assert!(!dns_impl::is_apt_based_rootfs(&rootfs));
    fs::create_dir_all(rootfs.join("etc").join("apt")).expect("create /etc/apt");
    assert!(dns_impl::is_apt_based_rootfs(&rootfs));
}

#[test]
fn is_apt_based_rootfs_detects_apt_get_binary() {
    let rootfs = temp_rootfs("apt-detect-bin");
    assert!(!dns_impl::is_apt_based_rootfs(&rootfs));
    fs::create_dir_all(rootfs.join("usr").join("bin")).expect("create /usr/bin");
    fs::write(rootfs.join("usr").join("bin").join("apt-get"), b"").expect("create apt-get");
    assert!(dns_impl::is_apt_based_rootfs(&rootfs));
}

/// The identity of a file as the filesystem records it, so a test can tell whether a
/// write happened at all.
///
/// A write that puts back the content that was already there is still a write: it
/// updates the modification and change times, and inside a workspace it also copies the
/// file up out of the shared lower layer. The timestamps are therefore the evidence,
/// not the content, which the functions under test preserve either way.
///
/// The kernel stamps a file from a clock that advances once per timer tick, so two
/// writes inside the same tick carry the same timestamp and are indistinguishable this
/// way. The tests that use this therefore wait past a tick before the write they are
/// looking for, which is what the sleep in each of them is for. The wait is a few
/// milliseconds and it is only about the clock: the content is unchanged either way,
/// which is exactly why the clock is the only thing that can answer the question.
fn file_identity(path: &Path) -> (i64, i64, i64, i64) {
    let metadata = fs::metadata(path).expect("read file metadata");
    (
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[test]
fn provisioning_the_same_hosts_content_twice_does_not_write_the_file_again() {
    let rootfs = temp_rootfs("dns-hosts-nochange");
    dns_impl::provision_etc_hosts(&rootfs).expect("first provision");
    let hosts = rootfs.join("etc").join("hosts");
    // Wait past a timer tick, so a write that happened would be visible in the
    // timestamps rather than sharing the tick with the first one.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let before = file_identity(&hosts);

    dns_impl::provision_etc_hosts(&rootfs).expect("second provision");

    assert_eq!(
        file_identity(&hosts),
        before,
        "a start that changes nothing about /etc/hosts must not write it"
    );
}

#[test]
fn provisioning_the_same_resolver_content_twice_does_not_write_the_file_again() {
    let rootfs = temp_rootfs("dns-resolv-nochange");
    dns_impl::provision_resolv_conf(&rootfs).expect("first provision");
    let resolv = rootfs.join("etc").join("resolv.conf");
    // Wait past a timer tick, so a write that happened would be visible in the
    // timestamps rather than sharing the tick with the first one.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let before = file_identity(&resolv);

    dns_impl::provision_resolv_conf(&rootfs).expect("second provision");

    assert_eq!(
        file_identity(&resolv),
        before,
        "a start with an unchanged resolver source must not write resolv.conf"
    );
}

#[test]
fn a_resolver_file_that_no_longer_matches_is_rewritten() {
    let rootfs = temp_rootfs("dns-resolv-changed");
    dns_impl::provision_resolv_conf(&rootfs).expect("first provision");
    let resolv = rootfs.join("etc").join("resolv.conf");
    let generated = fs::read_to_string(&resolv).expect("read the generated file");

    // Something else wrote the file, which is what a resolver source that changed
    // between two starts leaves behind.
    fs::write(&resolv, "nameserver 203.0.113.1\n").expect("replace resolv.conf");
    // Wait past a timer tick, so the regeneration is not stamped with the tick the
    // replacement above was stamped with.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let before = file_identity(&resolv);

    dns_impl::provision_resolv_conf(&rootfs).expect("reprovision");

    assert_eq!(
        fs::read_to_string(&resolv).expect("read resolv.conf"),
        generated,
        "a resolv.conf that no longer matches must be regenerated"
    );
    assert_ne!(
        file_identity(&resolv),
        before,
        "the regeneration must be a real write"
    );
}
