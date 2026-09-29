//! The providers Enclave knows, and the environment variable each maps to.
//!
//! The set is fixed rather than configurable: a provider is only useful if
//! Enclave knows which variable to export for it, and the wrapper that runs
//! inside a workspace is generated from this table. A credential that is not one
//! of these is an environment token instead, named by the variable a workspace
//! asks for; see [`super::names`].

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
