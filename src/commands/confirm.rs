//! Asking the operator before a destructive command, and reporting afterwards.
//!
//! A destructive command prints what it is about to do, then requires two typed
//! confirmations. Two is deliberate: the first is "yes, do it", which is what an
//! operator answers reflexively, and the second has to be typed out, which is not.
//! A command run without a terminal reads end of file on the first prompt and
//! stops, so a script cannot confirm by accident.
//!
//! What a teardown could not release is printed to stderr rather than stdout. It
//! is not part of the result the command reports, and a caller reading stdout
//! must not mistake a partial teardown for a clean one.

use anyhow::Result;

use std::io::Write;

pub(crate) fn confirm_destructive_action(summary: &str, final_phrase: &str) -> Result<bool> {
    eprintln!("{summary}");
    tracing::warn!("{summary}");

    if !prompt_exact("Type 'y' then press Enter to continue: ", "y")? {
        return Ok(false);
    }

    let second_prompt = format!("Type '{}' then press Enter to confirm: ", final_phrase);
    if !prompt_exact(&second_prompt, final_phrase)? {
        return Ok(false);
    }

    Ok(true)
}

/// Report the host resources a force teardown could not release.
///
/// A force destroy removes the registry record either way, so these lines are
/// the only remaining description of what is still running. Normal mode never
/// reaches this with a non-empty list: the daemon fails the request instead.
pub(crate) fn report_retained_resources(entries: impl IntoIterator<Item = String>) -> usize {
    let entries = entries.into_iter().collect::<Vec<_>>();
    if entries.is_empty() {
        return 0;
    }
    eprintln!("retained host resources; finish with `enclave doctor --repair`:");
    for entry in &entries {
        eprintln!("  - {entry}");
    }
    entries.len()
}

/// Read one line and accept it only when it matches exactly.
fn prompt_exact(prompt: &str, expected: &str) -> Result<bool> {
    eprint!("{prompt}");
    std::io::stderr().flush()?;

    let mut input = String::new();
    let read = std::io::stdin().read_line(&mut input)?;
    if read == 0 {
        return Ok(false);
    }

    Ok(input.trim() == expected)
}
