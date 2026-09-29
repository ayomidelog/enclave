//! Removing injected token values from captured output.
//!
//! A command run in a workspace can print a token it holds — `env`, a failing
//! curl that echoes its headers, a script that logs what it was given. The
//! workspace is entitled to the token, but the output of the command travels
//! further than the workspace does: into a terminal, a log, a CI job, a bug
//! report. Scrubbing is what keeps the credential from travelling with it.

/// What a removed value is replaced with.
pub(super) const REDACTED: &str = "[REDACTED]";

/// Replace every occurrence of every secret in `text`.
///
/// One pass, matching the longest secret at each position. Longest-first is what
/// keeps one secret from leaving a fragment of another behind: if `abc` and
/// `abcdef` are both injected and the text holds `abcdef`, replacing `abc` first
/// would leave `[REDACTED]def`.
///
/// An empty secret is ignored rather than matched, because an empty needle
/// matches at every position and would redact the whole output.
pub fn scrub_secrets(text: &str, secrets: &[String]) -> String {
    let secrets: Vec<&[u8]> = secrets
        .iter()
        .map(String::as_bytes)
        .filter(|secret| !secret.is_empty())
        .collect();
    if secrets.is_empty() {
        return text.to_string();
    }

    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        let matched = secrets
            .iter()
            .filter(|secret| bytes[index..].starts_with(secret))
            .max_by_key(|secret| secret.len());
        match matched {
            Some(secret) => {
                out.push_str(REDACTED);
                index += secret.len();
            }
            None => {
                // Copied a character at a time rather than a byte at a time, so
                // output that is not ASCII survives unchanged.
                let character = text[index..]
                    .chars()
                    .next()
                    .expect("index is always on a character boundary here");
                out.push(character);
                index += character.len_utf8();
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "../../tests/src/auth/scrub.rs"]
mod tests;
