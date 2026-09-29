//! The token files on disk, and the checks that make reading one safe.
//!
//! A token file is a secret at rest, so it is only ever read after its
//! ownership and mode have been checked. The checks live here rather than at
//! each call site because every path that reads a token has to pass them, and a
//! caller that forgot would read a file another user could have replaced.
//!
//! The layout is:
//!
//! ```text
//! <state_dir>/auth/<provider>.token              the shared namespace
//! <state_dir>/auth/users/<user_id>/<provider>.token   one user's namespace
//! <state_dir>/auth/audit.log                     every store, revoke, and inject
//! ```

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};

use super::providers::{provider_env_var, validate_provider};
use super::scope::{validate_user_id, TokenScope};

pub(super) const AUTH_DIR_NAME: &str = "auth";
const USERS_DIR_NAME: &str = "users";

/// A token file that exists, with when it was stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredToken {
    pub provider: String,
    /// The file's modification time, which is when the token was last written.
    pub stored_at: SystemTime,
}

/// What a store did.
///
/// Overwriting is refused rather than done silently, so the two outcomes are
/// distinct rather than both being success: the caller has to know that the
/// token it just read from stdin was not written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreOutcome {
    Stored(PathBuf),
    /// A token is already stored and `force` was not given.
    Exists(PathBuf),
}

impl StoreOutcome {
    pub fn path(&self) -> &Path {
        match self {
            Self::Stored(path) | Self::Exists(path) => path,
        }
    }

    pub fn stored(&self) -> bool {
        matches!(self, Self::Stored(_))
    }
}

/// Write a token, refusing to replace one that is already there.
pub(super) fn store_token(
    state_dir: &Path,
    scope: &TokenScope,
    provider: &str,
    token: &str,
    force: bool,
) -> Result<StoreOutcome> {
    validate_provider(provider)?;
    let token_dir = ensure_token_dir(state_dir, scope)?;
    let token_path = token_path_for_provider(&token_dir, provider)?;
    if token_path.exists() && !force {
        return Ok(StoreOutcome::Exists(token_path));
    }
    // Atomic, so a reader never sees a half-written token, and durable, because
    // a token that survived the command's exit status is the whole point of
    // storing it.
    crate::fsutil::write_file_atomic(&token_path, token.as_bytes(), 0o600)
        .with_context(|| format!("failed to write token file {}", token_path.display()))?;
    Ok(StoreOutcome::Stored(token_path))
}

pub(super) fn load_token(
    state_dir: &Path,
    scope: &TokenScope,
    provider: &str,
) -> Result<Option<String>> {
    validate_provider(provider)?;
    let Some(token_dir) = token_dir_if_exists(state_dir, scope)? else {
        return Ok(None);
    };
    let token_path = token_path_for_provider(&token_dir, provider)?;
    if !token_path.exists() {
        return Ok(None);
    }
    validate_token_permissions(&token_path)?;
    let raw = fs::read_to_string(&token_path)
        .with_context(|| format!("failed to read token file {}", token_path.display()))?;
    Ok(Some(raw.trim_end_matches('\n').to_string()))
}

pub(super) fn token_exists(state_dir: &Path, scope: &TokenScope, provider: &str) -> Result<bool> {
    validate_provider(provider)?;
    let Some(token_dir) = token_dir_if_exists(state_dir, scope)? else {
        return Ok(false);
    };
    let token_path = token_path_for_provider(&token_dir, provider)?;
    Ok(token_path.exists())
}

pub(super) fn delete_token(state_dir: &Path, scope: &TokenScope, provider: &str) -> Result<bool> {
    validate_provider(provider)?;
    let Some(token_dir) = token_dir_if_exists(state_dir, scope)? else {
        return Ok(false);
    };
    let token_path = token_path_for_provider(&token_dir, provider)?;
    if !token_path.exists() {
        return Ok(false);
    }
    // Removed, not truncated: the file is what the workspace reads, so an
    // unlinked token is gone, while an empty file would still be injected.
    fs::remove_file(&token_path)
        .with_context(|| format!("failed to remove token file {}", token_path.display()))?;
    Ok(true)
}

/// The tokens stored in one namespace, with when each was stored.
pub(super) fn list_tokens(state_dir: &Path, scope: &TokenScope) -> Result<Vec<StoredToken>> {
    let Some(token_dir) = token_dir_if_exists(state_dir, scope)? else {
        return Ok(Vec::new());
    };
    let mut tokens = Vec::new();
    for entry in fs::read_dir(&token_dir)
        .with_context(|| format!("failed to read auth directory {}", token_dir.display()))?
    {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|file| file.to_str()) else {
            continue;
        };
        let Some(provider) = name.strip_suffix(".token") else {
            continue;
        };
        // A file whose mode is wrong is not usable, so it is not reported as
        // configured. Reporting it would say a workspace will get a token that
        // the loader will then refuse to read.
        if provider_env_var(provider).is_none() || validate_token_permissions(&path).is_err() {
            continue;
        }
        let metadata = fs::metadata(&path)
            .with_context(|| format!("failed to stat token file {}", path.display()))?;
        let stored_at = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        tokens.push(StoredToken {
            provider: provider.to_string(),
            stored_at,
        });
    }
    tokens.sort_by(|left, right| left.provider.cmp(&right.provider));
    tokens.dedup_by(|left, right| left.provider == right.provider);
    Ok(tokens)
}

/// Whether a token file is one this process may read.
///
/// The checks are the same ones the shared namespace has always had: a regular
/// file, not a symlink, owned by the effective uid, mode 0600. A symlink is
/// refused rather than followed because following one would read a file outside
/// the namespace the caller asked for.
pub fn validate_token_permissions(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat token file {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("refusing to use symlink token file {}", path.display());
    }
    if !metadata.is_file() {
        bail!("token path {} is not a regular file", path.display());
    }
    let expected_uid = unsafe { libc::geteuid() as u32 };
    if metadata.uid() != expected_uid {
        bail!(
            "token file {} must be owned by uid {}, found uid {}",
            path.display(),
            expected_uid,
            metadata.uid()
        );
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o600 {
        bail!(
            "token file {} must have mode 0600, found {:o}",
            path.display(),
            mode
        );
    }
    Ok(())
}

/// The directory a scope's tokens live in, when it exists.
pub(super) fn token_dir_if_exists(state_dir: &Path, scope: &TokenScope) -> Result<Option<PathBuf>> {
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
            // A user namespace is only readable when its directory is private,
            // so a directory anyone could have replaced is refused rather than
            // read.
            validate_private_dir(&user_dir)?;
            user_dir
        }
    };
    let base = fs::canonicalize(state_dir)
        .with_context(|| format!("failed to canonicalize state dir {}", state_dir.display()))?;
    crate::fsutil::ensure_path_within(&base, &token_dir, "token directory").map(Some)
}

/// The directory a scope's tokens live in, created if it is missing.
pub(super) fn ensure_token_dir(state_dir: &Path, scope: &TokenScope) -> Result<PathBuf> {
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

pub(super) fn auth_dir_if_exists(state_dir: &Path) -> Result<Option<PathBuf>> {
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

pub(super) fn ensure_auth_dir(state_dir: &Path) -> Result<PathBuf> {
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

pub fn token_path_for_provider(auth_dir: &Path, provider: &str) -> Result<PathBuf> {
    validate_provider(provider)?;
    let file = format!("{provider}.token");
    Ok(auth_dir.join(file))
}

/// Create a directory that only its owner can enter, and keep it that way.
///
/// `ensure_secure_dir` only tightens a directory that is group- or
/// world-*writable*, which leaves a directory created under the usual umask at
/// 0755. A token directory has to be 0700 whatever the umask is, so the mode is
/// set here rather than inferred.
fn ensure_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("{} must not be a symlink", path.display());
    }
    if !metadata.is_dir() {
        bail!("{} is not a directory", path.display());
    }
    let expected_uid = unsafe { libc::geteuid() as u32 };
    if metadata.uid() != expected_uid {
        bail!(
            "{} must be owned by uid {}, found uid {}",
            path.display(),
            expected_uid,
            metadata.uid()
        );
    }
    if metadata.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to set mode 0700 on {}", path.display()))?;
    }
    Ok(())
}

/// Refuse a directory a token is about to be read from unless it is private.
fn validate_private_dir(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("{} must not be a symlink", path.display());
    }
    if !metadata.is_dir() {
        bail!("{} is not a directory", path.display());
    }
    let expected_uid = unsafe { libc::geteuid() as u32 };
    if metadata.uid() != expected_uid {
        bail!(
            "{} must be owned by uid {}, found uid {}",
            path.display(),
            expected_uid,
            metadata.uid()
        );
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o700 {
        bail!("{} must have mode 0700, found {:o}", path.display(), mode);
    }
    Ok(())
}
