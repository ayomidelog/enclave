//! Which namespace a token belongs to.
//!
//! A state directory used to have exactly one token per provider, so "the
//! github token" was unambiguous. A host running workspaces for several users
//! or several agents needs the token to belong to someone, so a token now lives
//! either in the state directory's own namespace or under a named user.

use anyhow::{bail, Result};

/// The longest user id accepted.
///
/// The id becomes one directory name, so the bound is what keeps a name from
/// being used to build a path that is longer than the filesystem allows.
const MAX_USER_ID_LEN: usize = 64;

/// Where a token is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenScope {
    /// The state directory's own namespace, at `<state_dir>/auth`.
    ///
    /// A workspace with no `owner` uses this, which is what every workspace did
    /// before user namespaces existed, so an owner-less workspace is unchanged.
    Shared,
    /// One user's namespace, at `<state_dir>/auth/users/<user_id>`.
    User(String),
}

impl TokenScope {
    /// The scope a workspace's `owner` field selects.
    pub fn for_owner(owner: Option<&str>) -> Self {
        match owner {
            Some(user_id) => Self::User(user_id.to_string()),
            None => Self::Shared,
        }
    }

    /// The user id, or `None` for the shared namespace.
    ///
    /// This is what an audit record carries, so a reader can tell a scoped token
    /// from the shared one rather than seeing both as an anonymous provider.
    pub fn user_id(&self) -> Option<&str> {
        match self {
            Self::Shared => None,
            Self::User(user_id) => Some(user_id),
        }
    }
}

/// Whether `user_id` is a name a token directory may be built from.
///
/// The rule is the URL-safe set the PRD asks for — alphanumerics and `+`, `-`,
/// `_`, `.` — which is also what makes the name safe as a path component: it
/// contains no separator and no character a shell would act on. `.` and `..`
/// pass the character rule and are rejected by name, because a directory built
/// from either would escape the namespace it is supposed to name.
pub fn validate_user_id(user_id: &str) -> Result<()> {
    if user_id.is_empty() {
        bail!("user id must not be empty");
    }
    if user_id.len() > MAX_USER_ID_LEN {
        bail!("user id must be at most {MAX_USER_ID_LEN} characters");
    }
    if user_id == "." || user_id == ".." {
        bail!("user id must not be '.' or '..'");
    }
    if !user_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '_' | '.'))
    {
        bail!("user id may only contain ASCII letters, digits, and '+', '-', '_', '.'");
    }
    Ok(())
}
