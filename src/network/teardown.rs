use std::process::Command;

use anyhow::Context;

pub fn remove_veth(veth_host: &str) {
    let status = Command::new("ip")
        .args(["link", "show", veth_host])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let exists = matches!(status, Ok(s) if s.success());
    if !exists {
        return;
    }

    if let Err(err) = delete_link(veth_host) {
        tracing::warn!("failed to delete veth {veth_host}: {err:#}");
    }
}

fn delete_link(name: &str) -> anyhow::Result<()> {
    let output = Command::new("ip")
        .args(["link", "delete", name])
        .output()
        .with_context(|| format!("failed to delete link {name}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ip link delete {name} failed: {}", stderr.trim());
    }
    Ok(())
}
