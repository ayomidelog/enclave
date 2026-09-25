use anyhow::{bail, Context, Result};

use crate::hostcmd::{HostCommand, HostOutput};

use super::bridge::BRIDGE_NAME;
use super::ipam;

pub fn setup_workspace_networking(
    pid: u32,
    workspace_ip: &str,
    veth_host: &str,
    veth_peer: &str,
) -> Result<()> {
    let result: Result<()> = (|| {
        let host_timer = crate::perf::Timer::new("network.veth.host");
        configure_host_veth(veth_host, veth_peer, pid)?;
        drop(host_timer);
        let netns_timer = crate::perf::Timer::new("network.veth.netns");
        configure_workspace_netns(pid, veth_peer, workspace_ip)?;
        drop(netns_timer);
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
    run_ip_batch(&host_veth_batch(host, peer, pid))
        .with_context(|| format!("host veth setup for {host} failed"))?;
    disable_ipv6(host);
    Ok(())
}

/// The `ip -batch` script that builds the host end of a workspace veth pair.
///
/// One ip process configures the whole pair instead of one per operation. On a
/// loaded host each spawn costs ~15 ms, which dominated workspace startup for a
/// handful of link commands.
///
/// The plan proposes replacing this with rtnetlink, and the measurement says not to.
/// A start now runs two ip processes for the whole of workspace networking, and the
/// spawn floor is about 4.5 ms each, so the total a netlink client could remove is
/// under 10 ms of a start that measures about 205 ms. What it would not remove is
/// the kernel's own work: measured on this host, one veth pair created and deleted is
/// 33 ms of kernel time against a 17 ms floor for the two spawns that did it. That is
/// the larger part of the phase, and it is the same either way. Against 10 ms stands
/// several hundred lines of hand-rolled netlink, or an async runtime this project
/// does not otherwise depend on, on the path that decides a workspace's network
/// isolation. The plan's own completion criterion for this item, that no shell is
/// spawned for standard workspace networking, is already met: `ip -batch` reads its
/// script from standard input and no shell is involved.
///
/// The peer is created directly inside the workspace's network namespace rather
/// than created here and moved into it afterwards. Moving an interface between
/// namespaces is the expensive half of building a pair: measured on this host, a
/// pair created here and moved costs 64.5 ms against 24.1 ms for the same pair with
/// the peer already in place, both including the deletion that follows. That is
/// about 40 ms off a start that measures 205 ms.
///
/// Naming the peer by its final name is what makes this possible. It is `eth0`
/// inside a namespace the session created, where nothing else exists, so there is no
/// name to collide with and no rename afterwards.
///
/// Port isolation goes through ip's `bridge_slave` type rather than the separate
/// `bridge` utility, so the host side stays one process. The `bridge`
/// subcommand cannot be reached from an `ip` batch, and that second spawn cost as
/// much as everything else on this side put together.
fn host_veth_batch(host: &str, peer: &str, pid: u32) -> String {
    format!(
        "link add {host} type veth peer name {peer} netns {pid}\n\
         link set {host} master {BRIDGE_NAME}\n\
         link set {host} type bridge_slave isolated on\n\
         link set {host} up\n"
    )
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

fn configure_workspace_netns(pid: u32, interface: &str, workspace_ip: &str) -> Result<()> {
    let pid_str = pid.to_string();
    let addr_cidr = format!("{workspace_ip}/24");
    // The final `route show default` both verifies the result and returns it in
    // the same process, so a healthy workspace pays one spawn for the whole
    // namespace configuration.
    let batch = format!(
        "link set lo up\n\
         addr add {addr_cidr} dev {interface}\n\
         link set {interface} up\n\
         route replace default via {} dev {interface}\n\
         route show default\n",
        ipam::GATEWAY_IP
    );
    let output = run_nsenter_ip_batch(&pid_str, &batch)
        .with_context(|| format!("failed to configure network namespace of pid {pid}"))?;
    let route_table = String::from_utf8_lossy(&output.stdout);
    if output.status.success()
        && default_route_output_has_route(&route_table, interface, ipam::GATEWAY_IP)
    {
        return Ok(());
    }

    let route_dump = dump_workspace_netns(&pid_str, &["route", "show"])
        .unwrap_or_else(|err| format!("failed to inspect route table: {err:#}"));
    let addr_dump = dump_workspace_netns(&pid_str, &["addr", "show", "dev", interface])
        .unwrap_or_else(|err| {
            format!("failed to inspect interface state for {interface}: {err:#}")
        });
    let stderr = String::from_utf8_lossy(&output.stderr);

    bail!(
        "workspace network namespace did not install the expected route for {} via {} ({}): {}\nroute table:\n{}\ninterface state:\n{}",
        interface,
        ipam::GATEWAY_IP,
        output.status,
        stderr.trim(),
        route_dump.trim(),
        addr_dump.trim()
    )
}

/// Feed `ip -batch` a command list on stdin and return its output.
fn run_ip_batch(commands: &str) -> Result<HostOutput> {
    run_batch(HostCommand::new("ip"), commands)
}

fn run_batch(command: HostCommand, commands: &str) -> Result<HostOutput> {
    command
        .args(["-batch", "-"])
        .stdin(commands.as_bytes().to_vec())
        .run()
        .context("failed to run network batch command")
}

fn run_nsenter_ip_batch(pid: &str, commands: &str) -> Result<HostOutput> {
    let pid = pid
        .parse::<u32>()
        .with_context(|| format!("invalid workspace pid '{pid}' for network setup"))?;
    run_batch(HostCommand::new("ip").netns(pid), commands)
}

fn run_ip(args: &[&str]) -> Result<()> {
    let output = run_ip_batch(&format!("{}\n", args.join(" ")))
        .with_context(|| format!("failed to run: ip {}", args.join(" ")))?;
    if !output.success() {
        bail!(
            "ip {} failed ({}): {}",
            args.join(" "),
            output.status,
            output.stderr_text()
        );
    }
    Ok(())
}

/// Dump the workspace namespace state for a setup failure.
///
/// The caller has already failed; this only runs to explain why, so it reports
/// its own failure as text rather than replacing the original error.
fn dump_workspace_netns(pid: &str, args: &[&str]) -> Result<String> {
    let pid_number = pid
        .parse::<u32>()
        .with_context(|| format!("invalid workspace pid '{pid}' for network diagnostics"))?;
    let output = HostCommand::new("ip")
        .netns(pid_number)
        .args(args)
        .run()
        .with_context(|| {
            format!(
                "failed to run diagnostic ip {} in the workspace namespace",
                args.join(" ")
            )
        })?;
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
