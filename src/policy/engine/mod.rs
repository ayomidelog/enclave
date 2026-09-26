//! The policy engine: who may run which action.
//!
//! A rule names a uid or a wildcard and an action pattern, and the decision is made in
//! a fixed order so it does not depend on the order rules were added. The policy is a
//! file, so it is read on every authorized request and cached by file identity; a
//! change made by an editor or another process is noticed rather than missed.
//!
//! The modules are split by the question each answers. `authorize` is the entry point
//! a request goes through, including the actions that skip it entirely. `decision`
//! evaluates a policy against a request and holds no state. `rules` changes the
//! policy. `store` reads and writes the file, and `cache` is the fingerprint that
//! keeps a read from re-parsing it.

mod authorize;
mod cache;
mod decision;
mod rules;
mod store;

pub use authorize::authorize;
pub use rules::{add_allow_rule, add_deny_rule, clear_rules, set_default_allow};
pub use store::{ensure_policy, load_policy};

#[cfg(test)]
#[path = "../../../tests/src/policy/engine.rs"]
mod tests;
