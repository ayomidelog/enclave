use std::process::Command;

use anyhow::{bail, Context, Result};

use super::bridge::BRIDGE_NAME;
use super::ipam;

const ROUTE_READY_TIMEOUT_MS: u64 = 2_000;
const ROUTE_READY_POLL_INTERVAL_MS: u64 = 50;

pub fn setup_workspace_networking(
    pid: u32,
    workspace_ip: &str,
    veth_host: &str,
    veth_peer: &str,
) -> Result<()> {
    let tmp_peer = format!("{veth_host}-p");
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

pub fn veth_names(host_octet: u8) -> (String, String) {
    (format!("veth-encl{host_octet}"), "eth0".to_string())
}

fn configure_host_veth(host: &str, peer: &str, pid: u32) -> Result<()> {
    let pid_str = pid.to_string();
    let script = r#"host="$1"
peer="$2"
bridge_name="$3"
target_pid="$4"

ip link add "$host" type veth peer name "$peer"
ip link set "$host" master "$bridge_name"
bridge link set dev "$host" isolated on
path="/proc/sys/net/ipv6/conf/${host}/disable_ipv6"
if [ -f "$path" ]; then
  printf '1' > "$path"
fi
ip link set "$host" up
ip link set "$peer" netns "$target_pid""#;
    let output = Command::new("sh")
        .arg("-ceu")
        .arg(script)
        .arg("sh")
        .arg(host)
        .arg(peer)
        .arg(BRIDGE_NAME)
        .arg(&pid_str)
        .output()
        .with_context(|| format!("failed to configure host veth setup for {host}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "host veth setup for {} failed ({}): {}",
            host,
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

fn configure_workspace_netns(
    pid: u32,
    old_name: &str,
    new_name: &str,
    workspace_ip: &str,
) -> Result<()> {
    let pid_str = pid.to_string();
    let addr_cidr = format!("{workspace_ip}/24");
    let script = r#"old_name="$1"
new_name="$2"
addr_cidr="$3"
gateway_ip="$4"
timeout_ms="$5"
poll_ms="$6"

ip link set "$old_name" name "$new_name"
for name in all default lo "$new_name"; do
  path="/proc/sys/net/ipv6/conf/${name}/disable_ipv6"
  if [ -f "$path" ]; then
    printf '1' > "$path"
  fi
done
ip link set lo up
ip addr add "$addr_cidr" dev "$new_name"
ip link set "$new_name" up
ip route replace default via "$gateway_ip" dev "$new_name"

elapsed_ms=0
while [ "$elapsed_ms" -lt "$timeout_ms" ]; do
  if ip route show default | grep -F "default via ${gateway_ip} dev ${new_name}" >/dev/null 2>&1; then
    exit 0
  fi
  sleep "0.$(printf '%03d' "$poll_ms")"
  elapsed_ms=$((elapsed_ms + poll_ms))
done
exit 1"#;
    let output = Command::new("nsenter")
        .arg("--net")
        .arg("--target")
        .arg(&pid_str)
        .arg("--")
        .arg("sh")
        .arg("-ceu")
        .arg(script)
        .arg("sh")
        .arg(old_name)
        .arg(new_name)
        .arg(&addr_cidr)
        .arg(ipam::GATEWAY_IP)
        .arg(ROUTE_READY_TIMEOUT_MS.to_string())
        .arg(ROUTE_READY_POLL_INTERVAL_MS.to_string())
        .output()
        .with_context(|| format!("failed to configure network namespace of pid {pid}"))?;
    if output.status.success() {
        return Ok(());
    }

    let route_dump = dump_nsenter_output(&pid_str, &["route", "show"])
        .unwrap_or_else(|err| format!("failed to inspect route table: {err:#}"));
    let addr_dump = dump_nsenter_output(&pid_str, &["addr", "show", "dev", new_name])
        .unwrap_or_else(|err| format!("failed to inspect interface state for {new_name}: {err:#}"));
    let stderr = String::from_utf8_lossy(&output.stderr);

    bail!(
        "workspace network namespace did not finish network setup for {} via {} within {} ms ({}): {}\nroute table:\n{}\ninterface state:\n{}",
        new_name,
        ipam::GATEWAY_IP,
        ROUTE_READY_TIMEOUT_MS,
        output.status,
        stderr.trim(),
        route_dump.trim(),
        addr_dump.trim()
    )
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
fn default_route_output_has_route(stdout: &str, iface: &str, gateway_ip: &str) -> bool {
    stdout
        .lines()
        .any(|line| line.contains("default") && line.contains(gateway_ip) && line.contains(iface))
}

#[cfg(test)]
#[path = "../../tests/src/network/veth.rs"]
mod tests;
