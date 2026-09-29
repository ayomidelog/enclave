//! Managing the provider tokens a workspace is given.
//!
//! The module is split by the command each part serves: `login` is the
//! interactive path, `store` the non-interactive one a script uses, `list` and
//! `logout` are the read and revoke halves, and `token_input` is the one place a
//! token value is read.

mod list;
mod login;
mod logout;
mod store;
mod token_input;

use anyhow::Result;

use crate::auth::{AuthManager, TokenScope};
use crate::cli::AuthCommands;

pub(crate) fn run_auth_command(command: AuthCommands) -> Result<()> {
    match command {
        AuthCommands::Login(args) => {
            let manager = AuthManager::new(args.state.state_dir.clone());
            login::run(&manager, args)
        }
        AuthCommands::Store(args) => {
            let manager = AuthManager::new(args.state.state_dir.clone());
            store::run(&manager, args)
        }
        AuthCommands::List(args) => {
            let manager = AuthManager::new(args.state.state_dir.clone());
            list::run(&manager, args)
        }
        AuthCommands::Logout(args) => {
            let manager = AuthManager::new(args.state.state_dir.clone());
            logout::run(&manager, args)
        }
    }
}

/// The namespace a command operates on, from its optional `--user`.
///
/// No user means the state directory's shared namespace, which is what every
/// command did before namespaces existed, so an invocation without `--user`
/// behaves as it always has.
fn scope_from(user: Option<&str>) -> TokenScope {
    TokenScope::for_owner(user)
}
