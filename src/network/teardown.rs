use std::process::Command;

use anyhow::{bail, Context, Result};

pub fn remove_veth(veth_host: &str) -> Result<()> {
    let output = Command::new("ip")
        .args(["link", "delete", veth_host])
        .output()
        .with_context(|| format!("failed to delete veth {veth_host}"))?;
    if !output.status.success() {
        if link_is_already_absent(&output.stderr) {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ip link delete {veth_host} failed: {}", stderr.trim());
    }
    Ok(())
}

fn link_is_already_absent(stderr: &[u8]) -> bool {
    let message = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    message.contains("cannot find device")
        || message.contains("does not exist")
        || message.contains("no such device")
}

#[cfg(test)]
#[path = "../../tests/src/network/teardown.rs"]
mod tests;
