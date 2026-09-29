//! The names a credential is addressed by.
//!
//! Three names meet here and each answers to a different rule. A *provider name*
//! is one of the fixed providers, and the provider table is what decides it. A
//! *token name* is what a token file is called, which is a provider name or the
//! slot an environment token reads from. An *environment token name* is the
//! variable a workspace asks for, which is any well-formed variable name.
//!
//! The last two live together because they are one mapping: an environment token
//! is stored under a slot derived from its name, and the derivation is what lets
//! a name the provider table has never heard of be stored and injected at all.
//! The rules are here rather than at each call site because every path that turns
//! a declared name into a file name has to pass them, and a caller that forgot
//! would build a path out of whatever a workspace definition contained.

use anyhow::{bail, Result};

/// The longest environment token name accepted.
///
/// The name becomes a file name in the workspace's `/run/enclave/env`, so the
/// bound is what keeps a name from being used to build a path longer than the
/// filesystem allows.
const MAX_ENV_TOKEN_LEN: usize = 64;

/// Whether `name` is a safe name to build a token file name from.
///
/// This is the path rule rather than the semantic one: the name becomes one file
/// name component, so it may not be empty and may not contain a separator or
/// anything else that could change which file is written. Which names are worth
/// storing is a different question, and the caller that knows whether it is
/// naming a provider or an environment token's slot is the one that answers it.
///
/// `_` is not in the set even though it is a safe character, because a stored
/// name is the lowercased form of the environment token that reads it and `_` is
/// how a `-` is written there. Allowing both spellings would allow a name that no
/// environment token can ever ask for, which is a token that is stored and never
/// injected.
pub fn validate_token_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        bail!("token name must not be empty");
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        bail!("token name must start with a lowercase ASCII letter or digit");
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        bail!("token name may only contain lowercase ASCII letters, digits, and '-'");
    }
    Ok(())
}

/// Whether `env_token` is a name a workspace may ask for a credential under.
///
/// The rule is the variable name itself: uppercase letters, digits, and `_`. It
/// has to be uppercase because the wrapper inside the workspace exports whatever
/// it finds in `/run/enclave/env`, and it only exports a name that matches
/// `[A-Z0-9_]+`; anything else would be written and never read.
///
/// The first character has to be a letter rather than `_` for the same reason one
/// step removed: the store slot is derived from the name, a leading `_` derives a
/// slot beginning with `-`, and a token file may not be named that. A name that
/// could never resolve is rejected when it is declared rather than left to inject
/// nothing.
pub fn validate_env_token_name(env_token: &str) -> Result<()> {
    if env_token.is_empty() {
        bail!("environment token name must not be empty");
    }
    if env_token.len() > MAX_ENV_TOKEN_LEN {
        bail!("environment token name must be at most {MAX_ENV_TOKEN_LEN} characters");
    }
    let mut chars = env_token.chars();
    let first = chars.next().expect("the name was checked to be non-empty");
    if !first.is_ascii_uppercase() {
        bail!("environment token name must start with an ASCII uppercase letter");
    }
    if !chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
        bail!("environment token name may only contain ASCII uppercase letters, digits, and '_'");
    }
    Ok(())
}

/// The store slot an environment token reads its value from.
///
/// The mapping is the one a person can hold in their head: the name lowercased,
/// with `_` written as `-`. It is total for every name
/// [`validate_env_token_name`] accepts and injective over them, so two
/// environment tokens never read the same slot.
pub fn slot_for_env_token(env_token: &str) -> Result<String> {
    validate_env_token_name(env_token)?;
    Ok(env_token.to_ascii_lowercase().replace('_', "-"))
}

#[cfg(test)]
#[path = "../../tests/src/auth/names.rs"]
mod tests;
