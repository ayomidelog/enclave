//! Removing a stored token.
//!
//! Without `--user` this operates on the shared namespace, which is what the
//! command did before namespaces existed.

use anyhow::Result;

use crate::auth::AuthManager;
use crate::cli::AuthProviderArgs;

use super::scope_from;

pub(super) fn run(manager: &AuthManager, args: AuthProviderArgs) -> Result<()> {
    let scope = scope_from(args.user.as_deref());
    let removed = manager.delete_token(&scope, &args.provider)?;
    match (removed, args.user.as_deref()) {
        (true, Some(user)) => println!(
            "removed token for provider \"{}\" for user \"{}\"",
            args.provider, user
        ),
        (true, None) => println!("removed token for provider \"{}\"", args.provider),
        (false, Some(user)) => println!(
            "no token configured for provider \"{}\" for user \"{}\"",
            args.provider, user
        ),
        (false, None) => println!("no token configured for provider \"{}\"", args.provider),
    }
    Ok(())
}
