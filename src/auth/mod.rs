//! Credentials: what they are, where they live, and how a workspace gets one.
//!
//! The module is split by the question each part answers. `providers` is the
//! fixed set of providers and the environment variable each maps to, `names` is
//! the rules a credential's names have to satisfy, `scope` is which namespace a
//! token belongs to, `storage` is the token files at rest and the checks that
//! make reading one safe, `resolution` is which credentials a workspace is
//! entitled to, `inject` is writing them into a workspace's namespace, `audit` is
//! the record of what was done to one, `scrub` is removing a value from captured
//! output, and `manager` is the facade the rest of the tree calls.

mod audit;
mod inject;
mod manager;
mod names;
mod providers;
mod resolution;
mod scope;
mod scrub;
mod storage;

pub use audit::{AuditAction, AuditEvent};
pub use inject::workspace_env_wrapper_script;
pub use manager::{AuthManager, WorkspaceAuthTarget, WorkspaceAuthToken};
pub use names::{slot_for_env_token, validate_env_token_name, validate_token_name};
pub use providers::{provider_env_var, provider_for_env_var, supported_providers};
pub use scope::{validate_user_id, TokenScope};
pub use scrub::scrub_secrets;
pub use storage::{StoreOutcome, StoredToken};
