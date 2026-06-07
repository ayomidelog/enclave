use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::bridge::BRIDGE_NAME;
use super::ipam;

const ROUTE_READY_TIMEOUT: Duration = Duration::from_secs(2);
const ROUTE_READY_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub fn setup_workspace_networking(
    pid: u32,
    workspace_ip: &str,
    veth_host: &str,
    veth_peer: &str,
) -> Result<()> {
    let tmp_peer = format!("{veth_host}-p");
    create_veth_pair(veth_host, &tmp_peer)?;
    let result: Result<()> = (|| {
        attach_to_bridge(veth_host)?;
        isolate_bridge_port(veth_host)?;
        super::bridge::disable_ipv6(veth_host)?;
        bring_up_host_side(veth_host)?;
        move_peer_to_netns(&tmp_peer, pid)?;
        rename_in_netns(pid, &tmp_peer, veth_peer)?;
        disable_ipv6_in_netns(pid, veth_peer)?;
        configure_netns(pid, veth_peer, workspace_ip)?;
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

pub fn veth_names(host_octet: u8) -> (String, String) {
    (format!("veth-encl{host_octet}"), "eth0".to_string())
}

fn create_veth_pair(host: &str, peer: &str) -> Result<()> {
    run_ip(&["link", "add", host, "type", "veth", "peer", "name", peer])
        .with_context(|| format!("failed to create veth pair {host} <-> {peer}"))
}

fn attach_to_bridge(host: &str) -> Result<()> {
    run_ip(&["link", "set", host, "master", BRIDGE_NAME])
        .with_context(|| format!("failed to attach {host} to bridge {BRIDGE_NAME}"))
}

fn bring_up_host_side(host: &str) -> Result<()> {
    run_ip(&["link", "set", host, "up"]).with_context(|| format!("failed to bring up {host}"))
}

fn isolate_bridge_port(host: &str) -> Result<()> {
    let output = Command::new("bridge")
        .args(["link", "set", "dev", host, "isolated", "on"])
        .output()
        .with_context(|| format!("failed to run: bridge link set dev {host} isolated on"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "bridge isolation for {} failed ({}): {}",
            host,
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

fn move_peer_to_netns(peer: &str, pid: u32) -> Result<()> {
    run_ip(&["link", "set", peer, "netns", &pid.to_string()])
        .with_context(|| format!("failed to move {peer} into netns of pid {pid}"))
}

fn rename_in_netns(pid: u32, old_name: &str, new_name: &str) -> Result<()> {
    run_nsenter_ip(
        &pid.to_string(),
        &["link", "set", old_name, "name", new_name],
    )
    .with_context(|| format!("failed to rename {old_name} to {new_name} inside netns of pid {pid}"))
}

fn configure_netns(pid: u32, iface: &str, workspace_ip: &str) -> Result<()> {
    let pid_str = pid.to_string();

    run_nsenter_ip(&pid_str, &["link", "set", "lo", "up"])?;

    let addr_cidr = format!("{workspace_ip}/24");
    run_nsenter_ip(&pid_str, &["addr", "add", &addr_cidr, "dev", iface])?;

    run_nsenter_ip(&pid_str, &["link", "set", iface, "up"])?;

    run_nsenter_ip(
        &pid_str,
        &[
            "route",
            "replace",
            "default",
            "via",
            ipam::GATEWAY_IP,
            "dev",
            iface,
        ],
    )?;
    wait_for_default_route(&pid_str, iface, ipam::GATEWAY_IP)?;

    Ok(())
}

fn wait_for_default_route(pid: &str, iface: &str, gateway_ip: &str) -> Result<()> {
    let started = Instant::now();
    while started.elapsed() < ROUTE_READY_TIMEOUT {
        if default_route_present(pid, iface, gateway_ip)? {
            return Ok(());
        }
        thread::sleep(ROUTE_READY_POLL_INTERVAL);
    }

    let route_dump = dump_nsenter_output(pid, &["route", "show"])
        .unwrap_or_else(|err| format!("failed to inspect route table: {err:#}"));
    let addr_dump = dump_nsenter_output(pid, &["addr", "show", "dev", iface])
        .unwrap_or_else(|err| format!("failed to inspect interface state for {iface}: {err:#}"));

    bail!(
        "workspace network namespace did not gain default route via {} dev {} within {} ms\nroute table:\n{}\ninterface state:\n{}",
        gateway_ip,
        iface,
        ROUTE_READY_TIMEOUT.as_millis(),
        route_dump.trim(),
        addr_dump.trim()
    );
}

fn default_route_present(pid: &str, iface: &str, gateway_ip: &str) -> Result<bool> {
    let output = run_nsenter_capture(
        pid,
        &["route", "show", "default"],
        &format!("failed to inspect default route inside netns of pid {pid}"),
    )?;
    if !output.status.success() {
        return Ok(false);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(default_route_output_has_route(&stdout, iface, gateway_ip))
}

fn default_route_output_has_route(stdout: &str, iface: &str, gateway_ip: &str) -> bool {
    stdout
        .lines()
        .any(|line| line.contains("default") && line.contains(gateway_ip) && line.contains(iface))
}

fn disable_ipv6_in_netns(pid: u32, iface: &str) -> Result<()> {
    let pid_str = pid.to_string();
    let script = r#"for name in all default lo "$1"; do
  path="/proc/sys/net/ipv6/conf/${name}/disable_ipv6"
  if [ -f "$path" ]; then
    printf '1' > "$path"
  fi
done"#;
    let output = Command::new("nsenter")
        .arg("--net")
        .arg("--target")
        .arg(&pid_str)
        .arg("--")
        .arg("sh")
        .arg("-ceu")
        .arg(script)
        .arg("sh")
        .arg(iface)
        .output()
        .with_context(|| format!("failed to disable IPv6 inside netns of pid {pid}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to disable IPv6 inside netns of pid {} ({}): {}",
            pid,
            output.status,
            stderr.trim()
        );
    }
    Ok(())
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

fn run_nsenter_ip(pid: &str, args: &[&str]) -> Result<()> {
    let output = run_nsenter_capture(
        pid,
        args,
        &format!(
            "failed to run: nsenter --net -t {pid} -- ip {}",
            args.join(" ")
        ),
    )?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "nsenter ip {} failed ({}): {}",
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

#[cfg(test)]
#[path = "../../tests/src/network/veth.rs"]
mod tests;
