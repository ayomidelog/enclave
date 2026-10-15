//! The policy and registry-repair requests.
//!
//! Both change host-wide state rather than the state of one sandbox or workspace,
//! so they take no selector and are grouped together.

use super::*;

pub(super) fn dispatch_registry_repair(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let strict = params
        .get("strict")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut report = registry::repair_registry(&config.state_dir, strict)?;
    // Repair fixes registry/disk consistency; this brings the persisted lifecycle
    // state back in line with what is actually running, so an interrupted start or
    // stop is recoverable without a daemon restart.
    report.reconciled_runtime_records = crate::sandbox::reconcile_runtime_state(&config.state_dir)?;
    Ok(serde_json::to_value(report)?)
}

pub(super) fn dispatch_policy_get(config: &DaemonConfig) -> Result<Value> {
    let current = policy::load_policy(&config.state_dir)?;
    Ok(serde_json::to_value(current)?)
}

pub(super) fn dispatch_policy_set_default(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let default_allow = params
        .get("default_allow")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow::anyhow!("missing 'default_allow'"))?;
    let updated = policy::set_default_allow(&config.state_dir, default_allow)?;
    Ok(serde_json::to_value(updated)?)
}

pub(super) fn dispatch_policy_clear(params: &Value, config: &DaemonConfig) -> Result<Value> {
    let uid = parse_uid(params)?;
    let updated = policy::clear_rules(&config.state_dir, uid)?;
    Ok(serde_json::to_value(updated)?)
}

pub(super) fn dispatch_policy_rule(
    params: &Value,
    config: &DaemonConfig,
    is_allow: bool,
) -> Result<Value> {
    let uid = parse_uid(params)?;
    let action = require_param_str(params, &["action"])?;
    let updated = if is_allow {
        policy::add_allow_rule(&config.state_dir, uid, action)?
    } else {
        policy::add_deny_rule(&config.state_dir, uid, action)?
    };
    Ok(serde_json::to_value(updated)?)
}

/// The uid a policy request names, when it names one.
///
/// A uid is a 32-bit value, so a larger one is a request the caller got wrong rather
/// than a rule to write. Casting it would wrap: `4294967296` would become a rule for
/// uid 0, and every uid differing only above 2^32 would collapse onto the same rule.
fn parse_uid(params: &Value) -> Result<Option<u32>> {
    let Some(value) = params.get("uid") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let raw = value
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("'uid' must be an unsigned integer"))?;
    let uid =
        u32::try_from(raw).map_err(|_| anyhow::anyhow!("'uid' must be a 32-bit uid, got {raw}"))?;
    Ok(Some(uid))
}
