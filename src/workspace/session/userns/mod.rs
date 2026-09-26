use std::collections::BTreeSet;
use std::env;
use std::ffi::CStr;
use std::fs;
use std::mem::MaybeUninit;
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};

const SUBUID_PATH: &str = "/etc/subuid";
const SUBGID_PATH: &str = "/etc/subgid";
const REQUIRED_SUBID_COUNT: u32 = 65_536;
const OWNER_OVERRIDE_ENV: &str = "ENCLAVE_SUBID_OWNER";
const DEFAULT_PASSWD_BUFFER_SIZE: usize = 1024;
static USER_NAMESPACE_MODE_CACHE: OnceLock<UserNamespaceMode> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdMapRange {
    pub inner_start: u32,
    pub outer_start: u32,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserNamespacePlan {
    pub owner: String,
    pub uid_map: IdMapRange,
    pub gid_map: IdMapRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserNamespaceMode {
    Enabled(UserNamespacePlan),
    Disabled,
}

pub fn detect_user_namespace_mode() -> Result<UserNamespaceMode> {
    if let Some(mode) = USER_NAMESPACE_MODE_CACHE.get() {
        return Ok(mode.clone());
    }

    let mode = detect_user_namespace_mode_uncached()?;
    let _ = USER_NAMESPACE_MODE_CACHE.set(mode.clone());
    Ok(mode)
}

fn detect_user_namespace_mode_uncached() -> Result<UserNamespaceMode> {
    let effective_uid = unsafe { libc::geteuid() as u32 };
    if effective_uid == 0 {
        return match subordinate_user_namespace_plan() {
            Ok(plan) if root_can_use_subordinate_plan(&plan) => {
                Ok(UserNamespaceMode::Enabled(plan))
            }
            Ok(plan) => {
                tracing::warn!(
                    "root-launched workspace session cannot use subordinate id owner '{}'; launching without a user namespace",
                    plan.owner
                );
                Ok(UserNamespaceMode::Disabled)
            }
            Err(err) => {
                tracing::warn!(
                    "root-launched workspace session has no usable root-owned subordinate id range; launching without a user namespace: {err:#}"
                );
                Ok(UserNamespaceMode::Disabled)
            }
        };
    }

    Ok(UserNamespaceMode::Enabled(
        subordinate_user_namespace_plan()?
    ))
}

fn subordinate_user_namespace_plan() -> Result<UserNamespacePlan> {
    let effective_uid = unsafe { libc::geteuid() as u32 };
    if effective_uid == 0 {
        tracing::debug!("attempting subordinate id user namespace plan for root-launched session");
    }
    let uid_ranges = parse_subordinate_id_file(SUBUID_PATH, "subuid")?;
    let gid_ranges = parse_subordinate_id_file(SUBGID_PATH, "subgid")?;
    subordinate_user_namespace_plan_from_ranges(&uid_ranges, &gid_ranges)
}
#[cfg(test)]
fn identity_user_namespace_plan(owner: &str, uid: u32, gid: u32) -> UserNamespacePlan {
    UserNamespacePlan {
        owner: owner.to_string(),
        uid_map: IdMapRange {
            inner_start: 0,
            outer_start: uid,
            count: 1,
        },
        gid_map: IdMapRange {
            inner_start: 0,
            outer_start: gid,
            count: 1,
        },
    }
}

fn root_can_use_subordinate_plan(plan: &UserNamespacePlan) -> bool {
    plan.owner == "root"
}

#[cfg(test)]
fn owner_name_or_uid(owner: Option<String>, uid: u32) -> String {
    owner.unwrap_or_else(|| format!("uid-{uid}"))
}

mod subid;

use subid::{parse_subordinate_id_file, subordinate_user_namespace_plan_from_ranges};
#[cfg(test)]
#[path = "../../../../tests/src/workspace/session/userns.rs"]
mod tests;
