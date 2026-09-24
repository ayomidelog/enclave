use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use super::bridge::BRIDGE_NAME;
use super::ipam;

pub fn setup_workspace_networking(
    pid: u32,
    workspace_ip: &str,
    veth_host: &str,
    veth_peer: &str,
) -> Result<()> {
    let tmp_peer = temporary_peer_name(veth_host);
    let result: Result<()> = (|| {
        configure_host_veth(veth_host, &tmp_peer, pid)?;
        configure_workspace_netns(pid, &tmp_peer, veth_peer, workspace_ip)?;
        Ok(())
    })();
    if let Err(err) = result {
        let cleanup_err = run_ip(&["link", "del", veth_host]).err();
        return Err(err).with_context(|| match cleanup_err {
            Some(cleanup_err) => format!(
                "failed to clean up partial veth setup for {}; cleanup failed: {}",
                veth_host, cleanup_err
            ),
            None => format!("cleaned up partial veth setup for {}", veth_host),
        });
    }
    Ok(())
}

fn temporary_peer_name(veth_host: &str) -> String {
    let hash = veth_host.bytes().fold(0x811c9dc5u32, |hash, byte| {
        hash.wrapping_mul(0x01000193) ^ u32::from(byte)
    });
    format!("vp{hash:08x}")
}

pub fn veth_names(host_octet: u8, workspace_id: &str) -> (String, String) {
    (
        format!("veth-{host_octet}-{:06x}", workspace_id_hash(workspace_id)),
        "eth0".to_string(),
    )
}

/// Recognize host veth names that Enclave's naming scheme produces.
///
/// Diagnostics use this to find interfaces that belong to Enclave without
/// depending on the registry, which may be missing or stale.
pub(crate) fn is_enclave_veth_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("veth-") else {
        return false;
    };
    let Some((octet, hash)) = rest.split_once('-') else {
        return false;
    };
    !octet.is_empty()
        && octet.len() <= 3
        && octet.bytes().all(|byte| byte.is_ascii_digit())
        && hash.len() == 6
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn workspace_id_hash(workspace_id: &str) -> u32 {
    workspace_id.bytes().fold(0x811c9dc5u32, |hash, byte| {
        hash.wrapping_mul(0x01000193) ^ u32::from(byte)
    }) & 0x00ff_ffff
}

fn configure_host_veth(host: &str, peer: &str, pid: u32) -> Result<()> {
    // One `ip` process configures the whole pair instead of one per operation.
    // On a loaded host each spawn costs ~15 ms, which dominated workspace
    // startup for a handful of link commands.
    run_ip_batch(&format!(
        "link add {host} type veth peer name {peer}\n\
         link set {host} master {BRIDGE_NAME}\n\
         link set {host} up\n\
         link set {peer} netns {pid}\n"
    ))
    .with_context(|| format!("host veth setup for {host} failed"))?;
    disable_ipv6(host);

    // Bridge port isolation is a `bridge` subcommand, so it needs its own
    // process; batching keeps it to one for the whole host side.
    let output = run_bridge_batch(&format!("link set dev {host} isolated on\n"))
        .with_context(|| format!("failed to isolate bridge port {host}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to isolate bridge port {} ({}): {}",
            host,
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

/// Best-effort IPv6 shutdown for the host end of the pair. The sysctl may be
/// absent on kernels built without IPv6, so a missing file is not an error.
fn disable_ipv6(interface: &str) {
    let path = format!("/proc/sys/net/ipv6/conf/{interface}/disable_ipv6");
    if std::path::Path::new(&path).exists() {
        if let Err(error) = std::fs::write(&path, "1") {
            tracing::debug!("failed to disable IPv6 on {interface}: {error}");
        }
    }
}

fn configure_workspace_netns(
    pid: u32,
    old_name: &str,
    new_name: &str,
    workspace_ip: &str,
) -> Result<()> {
    let pid_str = pid.to_string();
    let addr_cidr = format!("{workspace_ip}/24");
    // The final `route show default` both verifies the result and returns it in
    // the same process, so a healthy workspace pays one spawn for the whole
    // namespace configuration.
    let batch = format!(
        "link set {old_name} name {new_name}\n\
         link set lo up\n\
         addr add {addr_cidr} dev {new_name}\n\
         link set {new_name} up\n\
         route replace default via {} dev {new_name}\n\
         route show default\n",
        ipam::GATEWAY_IP
    );
    let output = run_nsenter_ip_batch(&pid_str, &batch)
        .with_context(|| format!("failed to configure network namespace of pid {pid}"))?;
    let route_table = String::from_utf8_lossy(&output.stdout);
    if output.status.success()
        && default_route_output_has_route(&route_table, new_name, ipam::GATEWAY_IP)
    {
        return Ok(());
    }

    let route_dump = dump_nsenter_output(&pid_str, &["route", "show"])
        .unwrap_or_else(|err| format!("failed to inspect route table: {err:#}"));
    let addr_dump = dump_nsenter_output(&pid_str, &["addr", "show", "dev", new_name])
        .unwrap_or_else(|err| format!("failed to inspect interface state for {new_name}: {err:#}"));
    let stderr = String::from_utf8_lossy(&output.stderr);

    bail!(
        "workspace network namespace did not install the expected route for {} via {} ({}): {}\nroute table:\n{}\ninterface state:\n{}",
        new_name,
        ipam::GATEWAY_IP,
        output.status,
        stderr.trim(),
        route_dump.trim(),
        addr_dump.trim()
    )
}

/// Feed `ip -batch` a command list on stdin and return its output.
fn run_ip_batch(commands: &str) -> Result<std::process::Output> {
    run_batch(&mut Command::new("ip"), commands)
}

fn run_bridge_batch(commands: &str) -> Result<std::process::Output> {
    run_batch(&mut Command::new("bridge"), commands)
}

fn run_batch(command: &mut Command, commands: &str) -> Result<std::process::Output> {
    let mut child = command
        .arg("-batch")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn network batch command")?;
    child
        .stdin
        .take()
        .context("network batch command has no stdin")?
        .write_all(commands.as_bytes())
        .context("failed to write network batch commands")?;
    child
        .wait_with_output()
        .context("failed to collect network batch output")
}

fn run_nsenter_ip_batch(pid: &str, commands: &str) -> Result<std::process::Output> {
    let mut command = Command::new("nsenter");
    command
        .arg("--net")
        .arg("--target")
        .arg(pid)
        .arg("--")
        .arg("ip");
    run_batch(&mut command, commands)
}

fn run_ip(args: &[&str]) -> Result<()> {
    let output = run_ip_batch(&format!("{}\n", args.join(" ")))
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

fn run_nsenter_capture(pid: &str, args: &[&str], context: &str) -> Result<std::process::Output> {
    let mut cmd = Command::new("nsenter");
    cmd.arg("--net").arg("--target").arg(pid).arg("--");
    cmd.arg("ip").args(args);
    cmd.output().with_context(|| context.to_string())
}

fn dump_nsenter_output(pid: &str, args: &[&str]) -> Result<String> {
    let output = run_nsenter_capture(
        pid,
        args,
        &format!(
            "failed to run diagnostic nsenter command: ip {}",
            args.join(" ")
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(format!(
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status, stdout, stderr
    ))
}

fn default_route_output_has_route(stdout: &str, iface: &str, gateway_ip: &str) -> bool {
    stdout
        .lines()
        .any(|line| line.contains("default") && line.contains(gateway_ip) && line.contains(iface))
}

#[cfg(test)]
#[path = "../../tests/src/network/veth.rs"]
mod tests;
