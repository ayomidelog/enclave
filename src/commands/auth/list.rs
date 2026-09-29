//! Listing stored tokens, by name and date.
//!
//! The listing is metadata only. A token value is never printed, so the command
//! is safe to run over a shoulder and safe to paste into a report.

use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};

use crate::auth::AuthManager;
use crate::cli::AuthListArgs;

use super::scope_from;

pub(super) fn run(manager: &AuthManager, args: AuthListArgs) -> Result<()> {
    let scope = scope_from(args.user.as_deref());
    let tokens = manager.list_tokens(&scope)?;
    let names: Vec<String> = tokens.iter().map(|token| token.provider.clone()).collect();

    match args.user.as_deref() {
        // The shared namespace keeps the listing it has always printed: every
        // provider, marked when it is configured.
        None => print!("{}", format_auth_provider_list(&names)),
        Some(user) => {
            let mut output = format!("Auth tokens for user \"{user}\":\n");
            if tokens.is_empty() {
                output.push_str("- (none stored)\n");
            }
            for token in &tokens {
                let stored_at: DateTime<Utc> = token.stored_at.into();
                output.push_str(&format!(
                    "- {}  stored {}\n",
                    token.provider,
                    stored_at.to_rfc3339_opts(SecondsFormat::Secs, true)
                ));
            }
            print!("{output}");
        }
    }
    Ok(())
}

/// Every provider Enclave supports, with the configured ones marked.
pub(super) fn format_auth_provider_list(configured: &[String]) -> String {
    let mut output = String::from("Supported auth providers:\n");
    for provider in crate::auth::supported_providers() {
        let configured_suffix = if configured.iter().any(|item| item == provider) {
            " (configured)"
        } else {
            ""
        };
        output.push_str(&format!("- {}{}\n", provider, configured_suffix));
    }
    output
}

#[cfg(test)]
#[path = "../../../tests/src/commands/auth.rs"]
mod tests;
