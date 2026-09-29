//! The token files on disk.
//!
//! The module is split by the question each part answers. `paths` is where a
//! scope's tokens live, `permissions` is the checks that make reading one safe,
//! and this file is the operations over them: store, load, list, and remove.
//!
//! A file is named by a token name, which is a provider name or the slot an
//! environment token reads from. The store itself does not care which: it holds
//! what it was given a name for, and the resolution that decides whether a name
//! can ever reach a workspace lives in `manager`.

mod paths;
mod permissions;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};

use super::names::validate_token_name;
use super::scope::TokenScope;

pub(in crate::auth) use paths::{ensure_auth_dir, token_dir_if_exists, token_path_for_name};
pub(in crate::auth) use permissions::validate_token_permissions;

/// A token file that exists, with when it was stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredToken {
    /// The name the token is stored under: a provider name, or the slot an
    /// environment token reads from.
    pub name: String,
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
    validate_token_name(provider)?;
    let token_dir = paths::ensure_token_dir(state_dir, scope)?;
    let token_path = token_path_for_name(&token_dir, provider)?;
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
    validate_token_name(provider)?;
    let Some(token_dir) = token_dir_if_exists(state_dir, scope)? else {
        return Ok(None);
    };
    let token_path = token_path_for_name(&token_dir, provider)?;
    if !token_path.exists() {
        return Ok(None);
    }
    validate_token_permissions(&token_path)?;
    let raw = fs::read_to_string(&token_path)
        .with_context(|| format!("failed to read token file {}", token_path.display()))?;
    Ok(Some(raw.trim_end_matches('\n').to_string()))
}

pub(super) fn token_exists(state_dir: &Path, scope: &TokenScope, provider: &str) -> Result<bool> {
    validate_token_name(provider)?;
    let Some(token_dir) = token_dir_if_exists(state_dir, scope)? else {
        return Ok(false);
    };
    let token_path = token_path_for_name(&token_dir, provider)?;
    Ok(token_path.exists())
}

pub(super) fn delete_token(state_dir: &Path, scope: &TokenScope, provider: &str) -> Result<bool> {
    validate_token_name(provider)?;
    let Some(token_dir) = token_dir_if_exists(state_dir, scope)? else {
        return Ok(false);
    };
    let token_path = token_path_for_name(&token_dir, provider)?;
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
        let Some(token_name) = name.strip_suffix(".token") else {
            continue;
        };
        // A file whose name or mode is wrong is not usable, so it is not reported
        // as configured. Reporting it would say a workspace will get a token that
        // the loader will then refuse to read.
        if validate_token_name(token_name).is_err() || validate_token_permissions(&path).is_err() {
            continue;
        }
        let metadata = fs::metadata(&path)
            .with_context(|| format!("failed to stat token file {}", path.display()))?;
        let stored_at = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        tokens.push(StoredToken {
            name: token_name.to_string(),
            stored_at,
        });
    }
    tokens.sort_by(|left, right| left.name.cmp(&right.name));
    tokens.dedup_by(|left, right| left.name == right.name);
    Ok(tokens)
}
