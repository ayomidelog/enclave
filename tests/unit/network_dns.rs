use std::fs;
use std::path::PathBuf;

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
