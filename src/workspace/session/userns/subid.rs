//! Choosing the subordinate id range a user namespace is mapped from.
//!
//! The kernel only maps ids the host has delegated, so this reads
//! `/etc/subuid` and `/etc/subgid`, picks the owner the rest of the system is
//! most likely to expect, and turns the pair of ranges into the uid and gid maps
//! a workspace session is launched with.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SubordinateIdRange {
    pub(super) owner: String,
    pub(super) start: u32,
    pub(super) count: u32,
}

pub(super) fn parse_subordinate_id_file(
    path: &str,
    label: &str,
) -> Result<Vec<SubordinateIdRange>> {
    let raw = fs::read_to_string(path).with_context(|| format!("failed to read {path}"))?;
    let mut ranges = Vec::new();

    for (line_no, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let mut parts = trimmed.split(':');
        let owner = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid {label} line {}", line_no + 1))?;
        let start = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid {label} line {}", line_no + 1))?
            .parse::<u32>()
            .with_context(|| format!("invalid {label} start value on line {}", line_no + 1))?;
        let count = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid {label} line {}", line_no + 1))?
            .parse::<u32>()
            .with_context(|| format!("invalid {label} count value on line {}", line_no + 1))?;
        if parts.next().is_some() {
            bail!("invalid {label} line {}: too many fields", line_no + 1);
        }
        ranges.push(SubordinateIdRange {
            owner: owner.to_string(),
            start,
            count,
        });
    }

    if ranges.is_empty() {
        bail!(
            "no {} entries found in {}; configure subordinate id ranges before starting workspaces",
            label,
            path
        );
    }

    Ok(ranges)
}

pub(super) fn choose_owner(
    uid_ranges: &[SubordinateIdRange],
    gid_ranges: &[SubordinateIdRange],
) -> Result<String> {
    let uid_owners: BTreeSet<&str> = uid_ranges
        .iter()
        .map(|entry| entry.owner.as_str())
        .collect();
    let gid_owners: BTreeSet<&str> = gid_ranges
        .iter()
        .map(|entry| entry.owner.as_str())
        .collect();
    let common: BTreeSet<&str> = uid_owners.intersection(&gid_owners).copied().collect();
    if common.is_empty() {
        bail!(
            "no common owner exists across {} and {}",
            SUBUID_PATH,
            SUBGID_PATH
        );
    }

    for owner in preferred_owners() {
        if common.contains(owner.as_str()) {
            return Ok(owner);
        }
    }

    if common.contains("root") {
        return Ok("root".to_string());
    }

    if common.len() == 1 {
        return Ok(common.iter().next().expect("one owner").to_string());
    }

    bail!(
        "unable to choose a subordinate id owner automatically from {}. Set {} to one of: {}",
        common.len(),
        OWNER_OVERRIDE_ENV,
        common.into_iter().collect::<Vec<_>>().join(", ")
    )
}

fn preferred_owners() -> Vec<String> {
    let effective_user = current_effective_username();
    preferred_owners_with_effective_user(effective_user.as_deref())
}

pub(super) fn preferred_owners_with_effective_user(effective_user: Option<&str>) -> Vec<String> {
    let mut owners = Vec::new();
    if let Ok(value) = env::var(OWNER_OVERRIDE_ENV) {
        push_owner_candidate(&mut owners, &value);
    }
    if let Some(value) = effective_user {
        push_owner_candidate(&mut owners, value);
    }
    for key in ["USER", "LOGNAME", "SUDO_USER"] {
        if let Ok(value) = env::var(key) {
            push_owner_candidate(&mut owners, &value);
        }
    }
    owners
}

fn push_owner_candidate(owners: &mut Vec<String>, value: &str) {
    let trimmed = value.trim();
    if !trimmed.is_empty() && !owners.iter().any(|existing| existing == trimmed) {
        owners.push(trimmed.to_string());
    }
}

fn current_effective_username() -> Option<String> {
    let euid = unsafe { libc::geteuid() };
    let suggested_len = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let buffer_len = if suggested_len <= 0 {
        DEFAULT_PASSWD_BUFFER_SIZE
    } else {
        suggested_len as usize
    };
    let mut buffer = vec![0u8; buffer_len];
    let mut passwd = MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let status = unsafe {
        libc::getpwuid_r(
            euid,
            passwd.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        return None;
    }

    let passwd = unsafe { passwd.assume_init() };
    let name = unsafe { CStr::from_ptr(passwd.pw_name) }
        .to_str()
        .ok()?
        .trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

pub(super) fn subordinate_user_namespace_plan_from_ranges(
    uid_ranges: &[SubordinateIdRange],
    gid_ranges: &[SubordinateIdRange],
) -> Result<UserNamespacePlan> {
    let owner = choose_owner(uid_ranges, gid_ranges)?;
    let uid_range = uid_ranges
        .iter()
        .find(|entry| entry.owner == owner)
        .ok_or_else(|| anyhow::anyhow!("missing subuid range for owner '{owner}'"))?;
    let gid_range = gid_ranges
        .iter()
        .find(|entry| entry.owner == owner)
        .ok_or_else(|| anyhow::anyhow!("missing subgid range for owner '{owner}'"))?;
    let count = uid_range.count.min(gid_range.count);
    if count < REQUIRED_SUBID_COUNT {
        bail!(
            "subordinate id range for '{}' is too small (need at least {}, got uid={} gid={})",
            owner,
            REQUIRED_SUBID_COUNT,
            uid_range.count,
            gid_range.count
        );
    }

    Ok(UserNamespacePlan {
        owner: owner.clone(),
        uid_map: IdMapRange {
            inner_start: 0,
            outer_start: uid_range.start,
            count: REQUIRED_SUBID_COUNT,
        },
        gid_map: IdMapRange {
            inner_start: 0,
            outer_start: gid_range.start,
            count: REQUIRED_SUBID_COUNT,
        },
    })
}
