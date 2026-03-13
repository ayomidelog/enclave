use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const ENCLAVE_DNS_HEADER: &str = "# Provisioned by Enclave";
const ENCLAVE_HOSTS_HEADER: &str = "# Provisioned by Enclave";
const ENCLAVE_APT_HEADER: &str = "// Provisioned by Enclave";
const SYSTEMD_RESOLVED_UPSTREAM: &str = "/run/systemd/resolve/resolv.conf";
const HOST_RESOLV_CONF: &str = "/etc/resolv.conf";
const APT_SANDBOX_OVERRIDE: &str = "APT::Sandbox::User \"root\";\n";
const FALLBACK_RESOLV_CONF: &str = "";
const DEFAULT_HOSTS_CONTENT: &str =
    "127.0.0.1 localhost\n::1 localhost ip6-localhost ip6-loopback\n";

pub fn provision_resolv_conf(rootfs: &Path) -> Result<()> {
    validate_rootfs_path(rootfs)?;

    let target_etc = ensure_rootfs_etc(rootfs)?;
    let target = target_etc.join("resolv.conf");
    let source = resolver_source_content()?;
    let content = format!(
        "{ENCLAVE_DNS_HEADER} — {source}\n{}",
        source.content.trim_end()
    );
    fs::write(&target, content).with_context(|| format!("failed to write {}", target.display()))?;
    Ok(())
}

pub fn provision_etc_hosts(rootfs: &Path) -> Result<()> {
    validate_rootfs_path(rootfs)?;
    let target_etc = ensure_rootfs_etc(rootfs)?;
    let target = target_etc.join("hosts");
    if target.exists() {
        return Ok(());
    }
    let content = format!("{ENCLAVE_HOSTS_HEADER}\n{DEFAULT_HOSTS_CONTENT}");
    fs::write(&target, content).with_context(|| format!("failed to write {}", target.display()))?;
    Ok(())
}

pub fn provision_apt_sandbox_override(rootfs: &Path) -> Result<()> {
    validate_rootfs_path(rootfs)?;
    if !is_apt_based_rootfs(rootfs) {
        tracing::debug!(
            "rootfs at {} does not appear to be APT-based; skipping apt sandbox override",
            rootfs.display()
        );
        return Ok(());
    }
    let apt_conf_dir = rootfs.join("etc").join("apt").join("apt.conf.d");
    fs::create_dir_all(&apt_conf_dir)
        .with_context(|| format!("failed to create {}", apt_conf_dir.display()))?;
    let target = apt_conf_dir.join("99enclave-nosandbox-user");
    let content = format!("{ENCLAVE_APT_HEADER}\n{APT_SANDBOX_OVERRIDE}");
    fs::write(&target, content).with_context(|| format!("failed to write {}", target.display()))?;
    Ok(())
}

pub fn is_apt_based_rootfs(rootfs: &Path) -> bool {
    rootfs.join("etc").join("apt").is_dir()
        || rootfs.join("usr").join("bin").join("apt-get").is_file()
}

struct ResolverSource {
    origin: String,
    content: String,
}

impl std::fmt::Display for ResolverSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "resolver list from {}", self.origin)
    }
}

fn resolver_source_content() -> Result<ResolverSource> {
    let host_resolv = Path::new(HOST_RESOLV_CONF);
    let host_content = read_text_if_exists(host_resolv)?;

    if let Some(content) = host_content {
        if contains_systemd_stub_resolver(&content) {
            let upstream = Path::new(SYSTEMD_RESOLVED_UPSTREAM);
            if let Some(upstream_content) = read_text_if_exists(upstream)? {
                return Ok(ResolverSource {
                    origin: SYSTEMD_RESOLVED_UPSTREAM.to_string(),
                    content: upstream_content,
                });
            }
            tracing::warn!(
                "host resolver is a systemd stub and {} is unavailable; keeping host resolver content",
                SYSTEMD_RESOLVED_UPSTREAM
            );
            return Ok(ResolverSource {
                origin: format!("{HOST_RESOLV_CONF} (systemd stub fallback)"),
                content,
            });
        }
        return Ok(ResolverSource {
            origin: HOST_RESOLV_CONF.to_string(),
            content,
        });
    }

    if let Some(upstream_content) = read_text_if_exists(Path::new(SYSTEMD_RESOLVED_UPSTREAM))? {
        return Ok(ResolverSource {
            origin: SYSTEMD_RESOLVED_UPSTREAM.to_string(),
            content: upstream_content,
        });
    }

    tracing::warn!("no host resolver configuration found; writing empty fallback resolv.conf");
    Ok(ResolverSource {
        origin: "empty fallback resolver config".to_string(),
        content: FALLBACK_RESOLV_CONF.to_string(),
    })
}

fn read_text_if_exists(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(Some(content))
}

fn contains_systemd_stub_resolver(content: &str) -> bool {
    content.lines().any(|line| {
        let mut parts = line.split_whitespace();
        matches!(
            (parts.next(), parts.next()),
            (Some("nameserver"), Some("127.0.0.53"))
        )
    })
}

fn ensure_rootfs_etc(rootfs: &Path) -> Result<PathBuf> {
    let target_etc = rootfs.join("etc");
    fs::create_dir_all(&target_etc)
        .with_context(|| format!("failed to create {}", target_etc.display()))?;
    Ok(target_etc)
}

fn validate_rootfs_path(rootfs: &Path) -> Result<()> {
    if !rootfs.is_absolute() {
        anyhow::bail!("rootfs path must be absolute: {}", rootfs.display());
    }
    if rootfs == Path::new("/") {
        anyhow::bail!("rootfs path must not be /");
    }

    let component_count = rootfs.components().count();
    if component_count < 3 {
        anyhow::bail!(
            "rootfs path is too shallow to be an Enclave-managed directory: {}",
            rootfs.display()
        );
    }
    Ok(())
}
