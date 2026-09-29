//! The providers Enclave knows, and the environment variable each maps to.
//!
//! The set is fixed rather than configurable: a provider is only useful if
//! Enclave knows which variable to export for it, and the wrapper that runs
//! inside a workspace is generated from this table.

use anyhow::{bail, Result};

/// Each provider, and the environment variable a workspace sees it as.
pub(super) const PROVIDERS: [(&str, &str); 3] = [
    ("enclave", "ENCLAVE_TOKEN"),
    ("github", "GITHUB_TOKEN"),
    ("npm", "NPM_TOKEN"),
];

/// Every provider Enclave can inject, in the order the wrapper exports them.
pub fn supported_providers() -> Vec<&'static str> {
    PROVIDERS.iter().map(|(provider, _)| *provider).collect()
}

/// The environment variable a provider is exported as.
pub fn provider_env_var(provider: &str) -> Option<&'static str> {
    PROVIDERS
        .iter()
        .find_map(|(name, env)| (*name == provider).then_some(*env))
}

/// The provider an environment variable belongs to.
pub fn provider_for_env_var(env_var: &str) -> Option<&'static str> {
    let normalized = env_var.trim().to_ascii_uppercase();
    PROVIDERS
        .iter()
        .find_map(|(name, env)| (*env == normalized).then_some(*name))
}

/// Whether Enclave can inject `provider` at all.
pub fn validate_provider(provider: &str) -> Result<()> {
    if provider_env_var(provider).is_none() {
        bail!(
            "unsupported auth provider '{}'; supported providers: {}",
            provider,
            supported_providers().join(", ")
        );
    }
    Ok(())
}

/// Whether `provider` is a safe name to build a file name from.
///
/// This is separate from [`validate_provider`] because the two answer different
/// questions. This one is the path rule: the name becomes one file name
/// component, so it may not be empty and may not contain a separator or anything
/// else that could change which file is written. The supported-provider check is
/// the semantic rule, and it is what decides whether a token can ever be
/// injected; both are applied, so a name has to be well formed *and* known.
pub fn validate_provider_name(provider: &str) -> Result<()> {
    let mut chars = provider.chars();
    let Some(first) = chars.next() else {
        bail!("provider name must not be empty");
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        bail!("provider name must start with a lowercase ASCII letter or digit");
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        bail!("provider name may only contain lowercase ASCII letters, digits, '-' and '_'");
    }
    Ok(())
}
