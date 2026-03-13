use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::ipam;

pub const BRIDGE_NAME: &str = "enclave0";

const SUBNET_PREFIX_LEN: &str = "24";

pub fn ensure_bridge() -> Result<()> {
    if bridge_exists()? {
        ensure_bridge_address()?;
        ensure_bridge_up()?;
        disable_ipv6(BRIDGE_NAME)?;
        return Ok(());
    }

    create_bridge()?;
    assign_bridge_address()?;
    ensure_bridge_up()?;
    disable_ipv6(BRIDGE_NAME)?;
    Ok(())
}

pub fn remove_bridge_if_idle() -> Result<bool> {
    if !bridge_exists()? {
        return Ok(true);
    }

    let output = Command::new("ip")
        .args(["link", "show", "master", BRIDGE_NAME])
        .output()
        .context("failed to list bridge members")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        return Ok(false);
    }

    run_ip(&["link", "set", BRIDGE_NAME, "down"])?;
    run_ip(&["link", "delete", BRIDGE_NAME, "type", "bridge"])?;
    Ok(true)
}

fn bridge_exists() -> Result<bool> {
    let status = Command::new("ip")
        .args(["link", "show", BRIDGE_NAME])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("failed to check bridge existence")?;
    Ok(status.success())
}

fn create_bridge() -> Result<()> {
    run_ip(&["link", "add", BRIDGE_NAME, "type", "bridge"])
        .with_context(|| format!("failed to create bridge {BRIDGE_NAME}"))
}

fn assign_bridge_address() -> Result<()> {
    let addr = format!("{}/{}", ipam::GATEWAY_IP, SUBNET_PREFIX_LEN);
    run_ip(&["addr", "add", &addr, "dev", BRIDGE_NAME])
        .with_context(|| format!("failed to assign address to {BRIDGE_NAME}"))
}

fn ensure_bridge_up() -> Result<()> {
    run_ip(&["link", "set", BRIDGE_NAME, "up"])
        .with_context(|| format!("failed to bring up {BRIDGE_NAME}"))
}

fn ensure_bridge_address() -> Result<()> {
    let output = Command::new("ip")
        .args(["addr", "show", "dev", BRIDGE_NAME])
        .output()
        .context("failed to inspect bridge address")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = format!("{}/{}", ipam::GATEWAY_IP, SUBNET_PREFIX_LEN);
    if stdout
        .lines()
        .any(|line| line.trim_start().starts_with("inet ") && line.contains(&expected))
    {
        return Ok(());
    }

    assign_bridge_address()
}

fn run_ip(args: &[&str]) -> Result<()> {
    let output = Command::new("ip")
        .args(args)
        .output()
        .with_context(|| format!("failed to run: ip {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "ip {} failed ({}): {}",
            args.join(" "),
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

pub fn disable_ipv6(interface: &str) -> Result<()> {
    let path = Path::new("/proc/sys/net/ipv6/conf")
        .join(interface)
        .join("disable_ipv6");
    if !path.exists() {
        return Ok(());
    }
    fs::write(&path, b"1")
        .with_context(|| format!("failed to disable IPv6 on interface {}", interface))?;
    Ok(())
}
