//! Storing a token without an interactive prompt.
//!
//! A script has no terminal to prompt at, so this path takes everything from
//! arguments except the token itself, which comes from standard input, and
//! reports its outcome in the exit status rather than in a question.

use anyhow::Result;

use crate::auth::{validate_token_name, validate_user_id, AuthManager};
use crate::cli::AuthStoreArgs;
use crate::commands::exit::{CliExit, EXIT_INVALID_INPUT, EXIT_IO, EXIT_TOKEN_EXISTS};

use super::token_input;

pub(super) fn run(manager: &AuthManager, args: AuthStoreArgs) -> Result<()> {
    // Validated here rather than by the argument parser so an unusable value
    // reports the documented status instead of the parser's usage status.
    validate_token_name(&args.provider)
        .map_err(|err| CliExit::error(EXIT_INVALID_INPUT, err.to_string()))?;
    validate_user_id(&args.user)
        .map_err(|err| CliExit::error(EXIT_INVALID_INPUT, err.to_string()))?;

    let token = token_input::read_token_line()
        .map_err(|err| CliExit::error(EXIT_IO, format!("{err:#}")))?;
    let token = token.trim();
    if token.is_empty() {
        return Err(CliExit::error(
            EXIT_INVALID_INPUT,
            "token must not be empty",
        ));
    }

    let scope = crate::auth::TokenScope::User(args.user.clone());
    let outcome = manager
        .store_token(&scope, &args.provider, token, args.force)
        .map_err(|err| CliExit::error(EXIT_IO, format!("{err:#}")))?;
    if !outcome.stored() {
        return Err(CliExit::error(
            EXIT_TOKEN_EXISTS,
            format!(
                "a token named \"{}\" is already stored for user \"{}\"; pass --force to replace it",
                args.provider, args.user
            ),
        ));
    }
    println!(
        "stored token \"{}\" for user \"{}\"",
        args.provider, args.user
    );
    Ok(())
}
