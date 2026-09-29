//! What a workspace definition has to satisfy.
//!
//! The rules are enforced here rather than at each call site, so the CLI,
//! the Enclavefile, and the daemon all get the same answer for the same
//! input.

use super::*;

pub(crate) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 63 {
        bail!("workspace name must be 1-63 characters");
    }

    let mut chars = name.chars();
    let first = chars
        .next()
        .ok_or_else(|| anyhow!("invalid workspace name"))?;
    if !first.is_ascii_alphanumeric() {
        bail!("workspace name must start with an ASCII letter or digit");
    }

    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            bail!("workspace name contains invalid character '{}'", c);
        }
    }
    Ok(())
}

pub(crate) fn normalize_auth_providers(auth_providers: Vec<String>) -> Result<Vec<String>> {
    let mut normalized = std::collections::BTreeSet::new();
    for provider in auth_providers {
        let provider = provider.trim().to_ascii_lowercase();
        if provider.is_empty() {
            continue;
        }
        crate::auth::provider_env_var(&provider).ok_or_else(|| {
            anyhow!(
                "unsupported auth provider '{}'; supported providers: {}",
                provider,
                crate::auth::supported_providers().join(", ")
            )
        })?;
        normalized.insert(provider);
    }
    Ok(normalized.into_iter().collect())
}

/// Validate the auth namespace a workspace's tokens are read from.
///
/// The id becomes a directory name under `<state_dir>/auth/users`, so it is
/// checked here rather than only where a token is stored: a workspace bound to
/// a namespace that could never hold a token would silently inject nothing.
pub(crate) fn normalize_owner(owner: Option<String>) -> Result<Option<String>> {
    let Some(owner) = owner else {
        return Ok(None);
    };
    let owner = owner.trim().to_string();
    if owner.is_empty() {
        return Ok(None);
    }
    crate::auth::validate_user_id(&owner)
        .map_err(|err| anyhow!("invalid workspace owner: {err}"))?;
    Ok(Some(owner))
}

/// Validate the environment tokens a workspace asks for.
///
/// Any well-formed variable name is accepted: a name that is not one of the
/// providers' is how a workspace asks for a credential the provider table does
/// not contain, and its value comes from the store slot the name derives. The
/// rule is checked here rather than only where a token is stored, because a name
/// that could never resolve would leave a workspace asking for a credential that
/// silently never arrives.
pub(crate) fn normalize_env_tokens(env_tokens: Vec<String>) -> Result<Vec<String>> {
    let mut normalized = std::collections::BTreeSet::new();
    for env_token in env_tokens {
        let env_token = env_token.trim().to_ascii_uppercase();
        if env_token.is_empty() {
            continue;
        }
        crate::auth::validate_env_token_name(&env_token)
            .map_err(|err| anyhow!("invalid environment token '{env_token}': {err}"))?;
        normalized.insert(env_token);
    }
    Ok(normalized.into_iter().collect())
}

pub(super) fn generate_workspace_id(name: &str) -> String {
    let slug = crate::fsutil::slugify(name, "workspace");
    let random = Uuid::new_v4().simple().to_string();
    format!("{slug}-{}", &random[..12])
}
