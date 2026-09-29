//! Removing a stored token.
//!
//! Without `--user` this operates on the shared namespace, which is what the
//! command did before namespaces existed.

use anyhow::Result;

use crate::auth::AuthManager;
use crate::cli::AuthTokenArgs;

use super::scope_from;

pub(super) fn run(manager: &AuthManager, args: AuthTokenArgs) -> Result<()> {
    let scope = scope_from(args.user.as_deref());
    let removed = manager.delete_token(&scope, &args.name)?;
    match (removed, args.user.as_deref()) {
        (true, Some(user)) => println!("removed token \"{}\" for user \"{}\"", args.name, user),
        (true, None) => println!("removed token \"{}\"", args.name),
        (false, Some(user)) => println!(
            "no token stored under \"{}\" for user \"{}\"",
            args.name, user
        ),
        (false, None) => println!("no token stored under \"{}\"", args.name),
    }
    Ok(())
}
