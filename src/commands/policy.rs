use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::cli::{PolicyClearArgs, PolicyCommands, PolicyDefaultArgs, PolicyRuleArgs};
use crate::policy::Policy;

use super::send_managed;

pub(crate) fn run_policy_command(socket: &Path, command: PolicyCommands) -> Result<()> {
    match command {
        PolicyCommands::Show => run_policy_show(socket),
        PolicyCommands::Default(args) => run_policy_default(socket, args),
        PolicyCommands::Allow(args) => run_policy_allow(socket, args),
        PolicyCommands::Deny(args) => run_policy_deny(socket, args),
        PolicyCommands::Clear(args) => run_policy_clear(socket, args),
    }
}

fn run_policy_show(socket: &Path) -> Result<()> {
    let response = send_managed(socket, "policy.get", json!({}))?;
    print_policy(serde_json::from_value(response)?);
    Ok(())
}

fn run_policy_default(socket: &Path, args: PolicyDefaultArgs) -> Result<()> {
    tracing::info!("setting default policy to '{}'...", args.mode);
    let response = send_managed(
        socket,
        "policy.set_default",
        json!({
            "default_allow": args.mode == "allow",
        }),
    )?;
    print_policy(serde_json::from_value(response)?);
    Ok(())
}

fn run_policy_allow(socket: &Path, args: PolicyRuleArgs) -> Result<()> {
    tracing::info!("adding allow rule for action '{}'...", args.action);
    let response = send_managed(
        socket,
        "policy.allow",
        json!({
            "uid": args.uid,
            "action": args.action,
        }),
    )?;
    print_policy(serde_json::from_value(response)?);
    Ok(())
}

fn run_policy_deny(socket: &Path, args: PolicyRuleArgs) -> Result<()> {
    tracing::info!("adding deny rule for action '{}'...", args.action);
    let response = send_managed(
        socket,
        "policy.deny",
        json!({
            "uid": args.uid,
            "action": args.action,
        }),
    )?;
    print_policy(serde_json::from_value(response)?);
    Ok(())
}

fn run_policy_clear(socket: &Path, args: PolicyClearArgs) -> Result<()> {
    tracing::info!("clearing policy rules...");
    let response = send_managed(
        socket,
        "policy.clear",
        json!({
            "uid": args.uid,
        }),
    )?;
    print_policy(serde_json::from_value(response)?);
    Ok(())
}

fn print_policy(policy: Policy) {
    println!(
        "policy: default_allow={}, rules={}",
        policy.default_allow,
        policy.rules.len()
    );
    if policy.rules.is_empty() {
        println!("no rules");
        return;
    }

    let rows: Vec<(String, String, String)> = policy
        .rules
        .into_iter()
        .map(|rule| {
            let uid = rule
                .uid
                .map(|v| v.to_string())
                .unwrap_or_else(|| "*".to_string());
            let allow = if rule.allow.is_empty() {
                "-".to_string()
            } else {
                rule.allow.join(",")
            };
            let deny = if rule.deny.is_empty() {
                "-".to_string()
            } else {
                rule.deny.join(",")
            };
            (uid, allow, deny)
        })
        .collect();

    let uid_width = rows
        .iter()
        .map(|(uid, _, _)| uid.len())
        .max()
        .unwrap_or(3)
        .max("UID".len());
    let allow_width = rows
        .iter()
        .map(|(_, allow, _)| allow.len())
        .max()
        .unwrap_or(5)
        .max("ALLOW".len());
    println!("{:<uid_width$} {:<allow_width$} DENY", "UID", "ALLOW");
    for (uid, allow, deny) in rows {
        println!("{:<uid_width$} {:<allow_width$} {}", uid, allow, deny);
    }
}
