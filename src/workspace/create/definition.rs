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
        crate::auth::provider_env_var(&provider)
            .ok_or_else(|| anyhow!("unsupported auth provider '{}'", provider))?;
        normalized.insert(provider);
    }
    Ok(normalized.into_iter().collect())
}

pub(crate) fn normalize_env_tokens(env_tokens: Vec<String>) -> Result<Vec<String>> {
    let mut normalized = std::collections::BTreeSet::new();
    for env_token in env_tokens {
        let env_token = env_token.trim().to_ascii_uppercase();
        if env_token.is_empty() {
            continue;
        }
        crate::auth::provider_for_env_var(&env_token)
            .ok_or_else(|| anyhow!("unsupported environment token '{}'", env_token))?;
        normalized.insert(env_token);
    }
    Ok(normalized.into_iter().collect())
}

pub(super) fn generate_workspace_id(name: &str) -> String {
    let slug = crate::fsutil::slugify(name, "workspace");
    let random = Uuid::new_v4().simple().to_string();
    format!("{slug}-{}", &random[..12])
}
