//! The interactive login.
//!
//! This is the path a person uses at a terminal. It prompts, hides what is typed,
//! and asks before replacing a token that is already stored, which is the
//! opposite of `store`: that one is for a script and refuses to overwrite unless
//! it is told to.

use std::io::Write;

use anyhow::{bail, Context, Result};

use crate::auth::AuthManager;
use crate::cli::AuthProviderArgs;

use super::scope_from;
use super::token_input;

pub(super) fn run(manager: &AuthManager, args: AuthProviderArgs) -> Result<()> {
    let scope = scope_from(args.user.as_deref());
    if manager.token_exists(&scope, &args.provider)? && !confirm_overwrite(&args.provider)? {
        println!("aborted");
        return Ok(());
    }

    token_input::prompt_for(&args.provider, args.user.as_deref());
    let token = token_input::read_token_line()?;
    if token.trim().is_empty() {
        bail!("token must not be empty");
    }
    manager.store_token(&scope, &args.provider, token.trim(), true)?;
    match args.user.as_deref() {
        Some(user) => println!(
            "stored token for provider \"{}\" for user \"{}\"",
            args.provider, user
        ),
        None => println!("stored token for provider \"{}\"", args.provider),
    }
    Ok(())
}

fn confirm_overwrite(provider: &str) -> Result<bool> {
    eprint!(
        "Token for provider \"{}\" already exists. Overwrite? [y/N]: ",
        provider
    );
    std::io::stderr().flush()?;
    let mut input = String::new();
    let read = std::io::stdin()
        .read_line(&mut input)
        .context("failed to read confirmation input")?;
    if read == 0 {
        return Ok(false);
    }
    Ok(matches!(input.trim(), "y" | "Y" | "yes" | "YES"))
}
