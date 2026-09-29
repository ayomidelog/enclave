//! The command line for token management.
//!
//! A token value is never an argument. It is read from standard input, which
//! keeps it out of the shell history and out of the process list, so no flag here
//! accepts one.

use std::path::PathBuf;

use clap::Args;

use super::common::{parse_token_name, parse_user_id_arg};
use super::default_state_dir_arg;

/// Which state directory a token command reads and writes.
///
/// The commands act on a state directory rather than on a running daemon, so
/// the directory has to be nameable: a host with a non-default one, and a test
/// that must not touch the real one, both need to say which.
#[derive(Args, Debug)]
pub struct AuthStateArgs {
    #[arg(long, value_name = "PATH", default_value_os_t = default_state_dir_arg())]
    pub state_dir: PathBuf,
}

/// Login and logout name a stored token positionally, and optionally a namespace.
#[derive(Args, Debug)]
#[command(about = "Store a token interactively, or remove one")]
pub struct AuthTokenArgs {
    #[command(flatten)]
    pub state: AuthStateArgs,
    /// The provider name, or the slot an environment token reads from.
    #[arg(value_name = "NAME", value_parser = parse_token_name)]
    pub name: String,
    /// Read and write this user's auth namespace instead of the shared one.
    #[arg(long, value_name = "USER_ID", value_parser = parse_user_id_arg)]
    pub user: Option<String>,
}

/// `--provider` and `--user` are validated by the command rather than by the
/// argument parser so that an unusable value reports the documented exit code
/// instead of the parser's generic usage status.
#[derive(Args, Debug)]
#[command(
    about = "Store a token non-interactively, reading the value from stdin",
    after_help = "\
The token is read from standard input, never from an argument, so it does not \
appear in the shell history or in the process list:

  printf '%s' \"$TOKEN\" | enclave auth store --user alice --provider github

Exit codes: 0 stored, 2 a token is already stored, 3 invalid provider or user, \
4 I/O error."
)]
pub struct AuthStoreArgs {
    #[command(flatten)]
    pub state: AuthStateArgs,
    /// The auth namespace to store into.
    #[arg(long, value_name = "USER_ID")]
    pub user: String,
    /// The provider name, or the slot an environment token reads from.
    #[arg(long, value_name = "NAME")]
    pub provider: String,
    /// Replace a token that is already stored.
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
#[command(about = "List stored tokens by provider and date, never by value")]
pub struct AuthListArgs {
    #[command(flatten)]
    pub state: AuthStateArgs,
    /// List this user's namespace instead of the shared one.
    #[arg(long, value_name = "USER_ID", value_parser = parse_user_id_arg)]
    pub user: Option<String>,
}
