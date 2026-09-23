use std::process::Command;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};

pub fn remove_veth(veth_host: &str) -> Result<()> {
    const MAX_ATTEMPTS: usize = 3;
    for attempt in 0..MAX_ATTEMPTS {
        let output = Command::new("ip")
            .args(["link", "delete", veth_host])
            .output()
            .with_context(|| format!("failed to delete veth {veth_host}"))?;
        if output.status.success() || link_is_already_absent(&output.stderr) {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !is_retryable_delete_error(&stderr) || attempt + 1 == MAX_ATTEMPTS {
            bail!("ip link delete {veth_host} failed: {}", stderr.trim());
        }
        tracing::debug!(
            "retrying veth deletion for {} after transient error (attempt {}/{})",
            veth_host,
            attempt + 1,
            MAX_ATTEMPTS
        );
        thread::sleep(Duration::from_millis(25 * (attempt as u64 + 1)));
    }
    unreachable!("veth deletion loop always returns or errors")
}

fn link_is_already_absent(stderr: &[u8]) -> bool {
    let message = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    message.contains("cannot find device")
        || message.contains("does not exist")
        || message.contains("no such device")
}

fn is_retryable_delete_error(stderr: &str) -> bool {
    let message = stderr.to_ascii_lowercase();
    message.contains("resource busy")
        || message.contains("temporarily unavailable")
        || message.contains("operation in progress")
}

#[cfg(test)]
#[path = "../../tests/src/network/teardown.rs"]
mod tests;
