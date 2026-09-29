//! Auth provider tokens: what they are, where they live, and how a workspace
//! gets one.
//!
//! The module is split by the question each part answers. `providers` is the
//! fixed set of providers and the environment variable each maps to, `scope` is
//! which namespace a token belongs to, `storage` is the token files at rest and
//! the checks that make reading one safe, `inject` is writing a resolved token
//! into a workspace's namespace, and `manager` is the facade the rest of the
//! tree calls.

mod inject;
mod manager;
mod providers;
mod scope;
mod storage;

pub use inject::workspace_env_wrapper_script;
pub use manager::{AuthManager, WorkspaceAuthToken};
pub use providers::{
    provider_env_var, provider_for_env_var, supported_providers, validate_provider,
    validate_provider_name,
};
pub use scope::{validate_user_id, TokenScope};
pub use storage::{StoreOutcome, StoredToken};
