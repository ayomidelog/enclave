//! Running the sandbox setup commands, with and without the setup cache.

use super::*;

pub(super) fn run_setup_commands(socket: &Path, ef: &Enclavefile, cache_setup: bool) -> Result<()> {
    if ef.sandbox.setup.is_empty() {
        return Ok(());
    }
    tracing::info!("running setup commands...");
    // The daemon keys the setup cache on the whole ordered list plus the sandbox
    // definition and the base rootfs, so it needs the list, not just a digest
    // computed here. The digest is still sent for a daemon that predates the
    // list and would otherwise refuse the request.
    let setup_digest = setup_digest(ef);
    for (i, cmd) in ef.sandbox.setup.iter().enumerate() {
        tracing::info!("  [{}/{}] {}", i + 1, ef.sandbox.setup.len(), cmd);
        let result = send(
            socket,
            "sandbox.exec_setup",
            json!({
                "sandbox": ef.sandbox.name,
                "command": cmd,
                "cache_setup": cache_setup,
                "setup_digest": setup_digest,
                "setup_commands": ef.sandbox.setup,
                "setup_index": i,
            }),
        );
        match result {
            Ok(value) => report_setup_outcome(i, cmd, &value),
            Err(err) => bail!("setup command failed: {}\n  command: {}", err, cmd),
        }
    }

    Ok(())
}

/// Say whether a setup command ran or was answered from the cache, and why.
///
/// The reason is only useful when setup caching is in play, so it goes through
/// tracing with the rest of the diagnostics rather than into stdout, which
/// callers may be parsing.
pub(super) fn report_setup_outcome(index: usize, command: &str, response: &serde_json::Value) {
    let Some(reason) = response.get("reason").and_then(serde_json::Value::as_str) else {
        return;
    };
    let cached = response
        .get("cached")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    tracing::info!(
        "  [{}] setup command {}: {}",
        index + 1,
        if cached { "cached" } else { "ran" },
        reason
    );
    if !cached {
        tracing::debug!("  [{}] ran: {}", index + 1, command);
    }
}

pub(super) fn setup_digest(ef: &Enclavefile) -> String {
    let mut digest = Sha256::new();
    digest.update(ef.sandbox.name.as_bytes());
    digest.update([0]);
    digest.update(ef.sandbox.suite.as_bytes());
    digest.update([0]);
    digest.update(ef.sandbox.bootstrap_method.to_string().as_bytes());
    for command in &ef.sandbox.setup {
        digest.update([0xff]);
        digest.update(command.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
