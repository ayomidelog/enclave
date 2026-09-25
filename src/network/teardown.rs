use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::hostcmd::HostCommand;

use super::NET_CLASS_DIR;

/// Whether an interface is one Enclave created for a workspace.
///
/// The name alone is not ownership: it is derived from a workspace id and an address, so
/// anything on the host could hold it. Two things together are the proof, and both are
/// readable from sysfs rather than remembered by Enclave: the name matches the scheme
/// Enclave generates, and the interface is enslaved to the Enclave bridge. A workspace host
/// end is created as a veth and attached to the bridge in the same batch, so an interface
/// with our name that is a dummy, or that is attached to something else, is not ours.
///
/// This is what makes removal safe to attempt by name. Without it, a teardown for a
/// workspace whose network was never built deletes an interface that merely happens to have
/// the name.
pub(crate) fn interface_is_workspace_veth(veth_host: &str) -> bool {
    super::veth::is_enclave_veth_name(veth_host) && super::bridge::is_bridge_member(veth_host)
}

pub fn remove_veth(veth_host: &str) -> Result<()> {
    const MAX_ATTEMPTS: usize = 3;
    // An interface with this name that Enclave did not create is left alone rather than
    // deleted: the name is derivable, so holding it is not evidence of ownership.
    if !interface_is_workspace_veth(veth_host) {
        if veth_is_present(veth_host) {
            tracing::warn!(
                "refusing to delete interface {veth_host}: it is not a workspace veth attached to {}",
                super::bridge::BRIDGE_NAME
            );
        }
        return Ok(());
    }
    // The interface's presence is the thing that matters, and it is readable
    // directly, so the already-gone case is decided by looking rather than by
    // matching the wording of whatever ip happened to print.
    if !veth_is_present(veth_host) {
        return Ok(());
    }
    for attempt in 0..MAX_ATTEMPTS {
        let output = HostCommand::new("ip")
            .args(["link", "delete", veth_host])
            .run()
            .with_context(|| format!("failed to delete veth {veth_host}"))?;
        if output.success() {
            return verify_veth_absent(veth_host);
        }
        // A concurrent teardown can remove the interface between the check above
        // and the delete call, so absence after a failure is still success.
        if !veth_is_present(veth_host) {
            return Ok(());
        }
        let stderr = output.stderr_text();
        if !is_retryable_delete_error(&stderr) || attempt + 1 == MAX_ATTEMPTS {
            // The decision was made from the text of the failure, so there is no
            // errno to report; the attempt count and the delays still are.
            crate::perf::record_cleanup_retry_exhausted(None);
            bail!("ip link delete {veth_host} failed: {stderr}");
        }
        tracing::debug!(
            "retrying veth deletion for {} after transient error (attempt {}/{})",
            veth_host,
            attempt + 1,
            MAX_ATTEMPTS
        );
        let delay = Duration::from_millis(25 * (attempt as u64 + 1));
        crate::perf::record_cleanup_retry();
        crate::perf::record_cleanup_retry_delay(delay.as_micros() as u64);
        thread::sleep(delay);
    }
    unreachable!("veth deletion loop always returns or errors")
}

/// Confirm the interface really disappeared instead of trusting `ip`'s exit
/// status. A veth can outlive the delete call when a namespace still holds it.
fn verify_veth_absent(veth_host: &str) -> Result<()> {
    if !veth_is_present(veth_host) {
        return Ok(());
    }
    for attempt in 0..2 {
        let delay = Duration::from_millis(25 * (attempt as u64 + 1));
        crate::perf::record_cleanup_retry();
        crate::perf::record_cleanup_retry_delay(delay.as_micros() as u64);
        thread::sleep(delay);
        if !veth_is_present(veth_host) {
            return Ok(());
        }
    }
    crate::perf::record_cleanup_retry_exhausted(None);
    bail!(
        "veth {veth_host} is still present in {} after deletion",
        NET_CLASS_DIR
    )
}

/// Whether an interface with this name exists on the host.
///
/// Destroy verification uses this to prove an interface is gone after its
/// removal, rather than trusting that the removal call returned success.
pub fn veth_is_present(veth_host: &str) -> bool {
    std::path::Path::new(NET_CLASS_DIR).join(veth_host).exists()
}

/// Whether a delete failure is worth retrying.
///
/// These are transient kernel conditions that ip only reports in text, and a
/// retry is harmless when the guess is wrong, so the wording is the whole
/// signal here.
fn is_retryable_delete_error(stderr: &str) -> bool {
    let message = stderr.to_ascii_lowercase();
    message.contains("resource busy")
        || message.contains("temporarily unavailable")
        || message.contains("operation in progress")
}

#[cfg(test)]
#[path = "../../tests/src/network/teardown.rs"]
mod tests;
