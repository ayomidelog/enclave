//! Asking the operator before a destructive command, and reporting afterwards.
//!
//! A destructive command prints what it is about to do, then requires two typed
//! confirmations. Two is deliberate: the first is "yes, do it", which is what an
//! operator answers reflexively, and the second has to be typed out, which is not.
//! A command run without a terminal reads end of file on the first prompt and
//! stops, so a script cannot confirm by accident.
//!
//! Stopping is not the same as succeeding, though, and a caller that reads only the
//! exit status cannot tell them apart. An answer of "no" is the operator choosing
//! not to act, which is a normal outcome. A prompt that could not be answered at all
//! is reported as a failure, because nothing was deleted and a script that took the
//! exit status for the result would go on as though the teardown had happened.
//!
//! What a teardown could not release is printed to stderr rather than stdout. It
//! is not part of the result the command reports, and a caller reading stdout
//! must not mistake a partial teardown for a clean one.

use anyhow::{bail, Result};

use std::io::Write;

/// What came back from a destructive prompt.
pub(crate) enum Confirmation {
    /// Both prompts were answered exactly as required.
    Confirmed,
    /// The operator answered, and the answer was not the required one.
    Declined,
    /// There was nothing to read, because standard input is not a terminal.
    Unanswerable,
}

pub(crate) fn confirm_destructive_action(
    summary: &str,
    final_phrase: &str,
) -> Result<Confirmation> {
    eprintln!("{summary}");
    tracing::warn!("{summary}");

    match prompt_exact("Type 'y' then press Enter to continue: ", "y")? {
        Answer::Matched => {}
        Answer::Declined => return Ok(Confirmation::Declined),
        Answer::Unanswerable => return Ok(Confirmation::Unanswerable),
    }

    let second_prompt = format!("Type '{}' then press Enter to confirm: ", final_phrase);
    match prompt_exact(&second_prompt, final_phrase)? {
        Answer::Matched => Ok(Confirmation::Confirmed),
        Answer::Declined => Ok(Confirmation::Declined),
        Answer::Unanswerable => Ok(Confirmation::Unanswerable),
    }
}

/// Turn a confirmation the caller did not get into its outcome.
///
/// A decline prints the same line it always has and reports success, because the
/// operator got what they asked for. A prompt that could not be answered is an
/// error: the alternative is a destructive command that deletes nothing, reports
/// nothing, and exits zero, which is how a script ends up believing a teardown
/// happened. The message names the way to do the same thing without a prompt.
pub(crate) fn require_confirmation(
    confirmation: Confirmation,
    action: &str,
    without_a_prompt: &str,
) -> Result<()> {
    match confirmation {
        Confirmation::Confirmed => Ok(()),
        Confirmation::Declined => {
            println!("aborted");
            Ok(())
        }
        Confirmation::Unanswerable => bail!(
            "{action} was not confirmed and nothing was deleted: the confirmation prompt could not be read because standard input is not a terminal. {without_a_prompt}"
        ),
    }
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
fn prompt_exact(prompt: &str, expected: &str) -> Result<Answer> {
    eprint!("{prompt}");
    std::io::stderr().flush()?;

    let mut input = String::new();
    let read = std::io::stdin().read_line(&mut input)?;
    Ok(interpret_answer(read, &input, expected))
}

/// What one prompt read back.
enum Answer {
    /// The line matched what the prompt asked for.
    Matched,
    /// The operator typed something else.
    Declined,
    /// The line could not be read at all.
    Unanswerable,
}

/// Decide what one prompt's read means.
///
/// Split from the read itself so the three outcomes are testable without a
/// terminal: a read of zero bytes is end of file, which is what a command run from
/// a script with no input reads, and it is the case that used to be reported as a
/// successful teardown.
fn interpret_answer(bytes_read: usize, line: &str, expected: &str) -> Answer {
    if bytes_read == 0 {
        return Answer::Unanswerable;
    }
    if line.trim() == expected {
        Answer::Matched
    } else {
        Answer::Declined
    }
}

#[cfg(test)]
#[path = "../../tests/src/commands/confirm.rs"]
mod tests;
