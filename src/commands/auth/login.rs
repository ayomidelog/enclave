//! The interactive login.
//!
//! This is the path a person uses at a terminal. It prompts, hides what is typed,
//! and asks before replacing a token that is already stored, which is the
//! opposite of `store`: that one is for a script and refuses to overwrite unless
//! it is told to.

use std::io::Write;

use anyhow::{bail, Context, Result};

use crate::auth::AuthManager;
use crate::cli::AuthTokenArgs;

use super::scope_from;
use super::token_input;

pub(super) fn run(manager: &AuthManager, args: AuthTokenArgs) -> Result<()> {
    let scope = scope_from(args.user.as_deref());
    if manager.token_exists(&scope, &args.name)? && !confirm_overwrite(&args.name)? {
        println!("aborted");
        return Ok(());
    }

    token_input::prompt_for(&args.name, args.user.as_deref());
    let token = token_input::read_token_line()?;
    if token.trim().is_empty() {
        bail!("token must not be empty");
    }
    manager.store_token(&scope, &args.name, token.trim(), true)?;
    match args.user.as_deref() {
        Some(user) => println!("stored token \"{}\" for user \"{}\"", args.name, user),
        None => println!("stored token \"{}\"", args.name),
    }
    Ok(())
}

fn confirm_overwrite(name: &str) -> Result<bool> {
    eprint!("Token \"{}\" already exists. Overwrite? [y/N]: ", name);
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
