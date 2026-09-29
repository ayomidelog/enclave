//! Where a scope's tokens live.
//!
//! The layout is the whole of this module: a scope maps to one directory, and a
//! provider maps to one file inside it. Keeping the two mappings here is what
//! makes the layout checkable in one place rather than at each call site.
//!
//! ```text
//! <state_dir>/auth/<provider>.token                   the shared namespace
//! <state_dir>/auth/users/<user_id>/<provider>.token   one user's namespace
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::super::providers::validate_provider;
use super::super::scope::{validate_user_id, TokenScope};
use super::permissions::ensure_private_dir;

pub(super) const AUTH_DIR_NAME: &str = "auth";
const USERS_DIR_NAME: &str = "users";

/// The directory a scope's tokens live in, when it exists.
pub(in crate::auth) fn token_dir_if_exists(
    state_dir: &Path,
    scope: &TokenScope,
) -> Result<Option<PathBuf>> {
    let Some(auth_dir) = auth_dir_if_exists(state_dir)? else {
        return Ok(None);
    };
    let token_dir = match scope {
        TokenScope::Shared => auth_dir,
        TokenScope::User(user_id) => {
            validate_user_id(user_id)?;
            let user_dir = auth_dir.join(USERS_DIR_NAME).join(user_id);
            if !user_dir.exists() {
                return Ok(None);
            }
            // A user namespace is only readable when its directory is private, so
            // a directory anyone could have replaced is refused rather than read.
            super::permissions::validate_private_dir(&user_dir)?;
            user_dir
        }
    };
    let base = fs::canonicalize(state_dir)
        .with_context(|| format!("failed to canonicalize state dir {}", state_dir.display()))?;
    crate::fsutil::ensure_path_within(&base, &token_dir, "token directory").map(Some)
}

/// The directory a scope's tokens live in, created if it is missing.
pub(in crate::auth) fn ensure_token_dir(state_dir: &Path, scope: &TokenScope) -> Result<PathBuf> {
    let auth_dir = ensure_auth_dir(state_dir)?;
    let state_dir = fs::canonicalize(state_dir)
        .with_context(|| format!("failed to canonicalize state dir {}", state_dir.display()))?;
    let token_dir = match scope {
        TokenScope::Shared => auth_dir,
        TokenScope::User(user_id) => {
            validate_user_id(user_id)?;
            let users_dir = auth_dir.join(USERS_DIR_NAME);
            ensure_private_dir(&users_dir)?;
            let user_dir = users_dir.join(user_id);
            ensure_private_dir(&user_dir)?;
            user_dir
        }
    };
    crate::fsutil::ensure_path_within(&state_dir, &token_dir, "token directory")
}

pub(in crate::auth) fn auth_dir_if_exists(state_dir: &Path) -> Result<Option<PathBuf>> {
    if !state_dir.exists() {
        return Ok(None);
    }
    let state_dir = fs::canonicalize(state_dir)
        .with_context(|| format!("failed to canonicalize state dir {}", state_dir.display()))?;
    let auth_dir = state_dir.join(AUTH_DIR_NAME);
    if !auth_dir.exists() {
        return Ok(None);
    }
    let auth_dir = crate::fsutil::ensure_path_within(&state_dir, &auth_dir, "auth directory")?;
    Ok(Some(auth_dir))
}

pub(in crate::auth) fn ensure_auth_dir(state_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create state dir {}", state_dir.display()))?;
    crate::fsutil::ensure_secure_dir(state_dir)?;
    let state_dir = fs::canonicalize(state_dir)
        .with_context(|| format!("failed to canonicalize state dir {}", state_dir.display()))?;

    let auth_dir = state_dir.join(AUTH_DIR_NAME);
    fs::create_dir_all(&auth_dir)
        .with_context(|| format!("failed to create auth dir {}", auth_dir.display()))?;
    crate::fsutil::ensure_secure_dir(&auth_dir)?;
    crate::fsutil::ensure_path_within(&state_dir, &auth_dir, "auth directory")
}

/// The file one provider's token lives in, inside a namespace directory.
pub(in crate::auth) fn token_path_for_provider(auth_dir: &Path, provider: &str) -> Result<PathBuf> {
    validate_provider(provider)?;
    let file = format!("{provider}.token");
    Ok(auth_dir.join(file))
}
