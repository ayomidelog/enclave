use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

use crate::network::{ipam, nat, veth};

use super::DoctorCheck;

const NET_CLASS_DIR: &str = "/sys/class/net";

/// Inventory the host networking Enclave claims to own.
///
/// A registry entry with an assigned IP implies a veth pair and two
/// anti-spoofing rules. Anything present on the host without a matching
/// registry entry is a leak; anything expected but missing means a workspace
/// is running without the isolation Enclave promised.
pub(super) fn check_workspace_network(state_dir: &Path) -> DoctorCheck {
    const NAME: &str = "workspace_network";

    let expected = match expected_network_owners(state_dir) {
        Ok(expected) => expected,
        Err(err) => {
            return DoctorCheck::warn(
                NAME,
                &format!("failed to read workspace network ownership: {err:#}"),
            )
        }
    };

    let present = match host_enclave_veth_names(Path::new(NET_CLASS_DIR)) {
        Ok(present) => present,
        Err(err) => {
            return DoctorCheck::warn(NAME, &format!("failed to list host interfaces: {err}"))
        }
    };

    let diff = veth_inventory_diff(&expected, &present);
    let rules = match nat::list_owned_rules() {
        Ok(rules) => rules,
        Err(err) => {
            return DoctorCheck::warn(NAME, &format!("firewall rule inventory unknown: {err:#}"))
        }
    };
    let owners = expected.values().cloned().collect::<BTreeSet<_>>();
    let leaked_owners = leaked_rule_owners(&owners, &rules);

    if diff.is_clean() && leaked_owners.is_empty() {
        return DoctorCheck::ok(
            NAME,
            &format!(
                "{} workspace veth(s) and {} owned firewall rule(s) accounted for",
                expected.len(),
                rules.len()
            ),
        );
    }

    let mut details = Vec::new();
    if !diff.leaked_veths.is_empty() {
        // Enclave can be run with more than one state directory on a host, and
        // this check only sees the registry it was given. Report these as
        // unaccounted for rather than as leaks so the message is not a claim
        // the doctor cannot support.
        details.push(format!(
            "{} veth(s) not owned by this state directory (another Enclave state \
             directory, or a leak): {}",
            diff.leaked_veths.len(),
            diff.leaked_veths.join(", ")
        ));
    }
    if !diff.missing_veths.is_empty() {
        details.push(format!(
            "{} running workspace(s) missing their veth: {}",
            diff.missing_veths.len(),
            diff.missing_veths.join(", ")
        ));
    }
    if !leaked_owners.is_empty() {
        details.push(format!(
            "{} firewall rule(s) reference workspace(s) unknown to this state \
             directory: {}",
            leaked_owners.len(),
            leaked_owners.join(", ")
        ));
    }
    DoctorCheck::warn(NAME, &details.join("; "))
}

/// Report a host interface that already owns part of the Enclave subnet.
///
/// Enclave builds its workspace network on a fixed subnet, so a host address
/// inside it gives the kernel two connected routes for one prefix and makes
/// workspace routing ambiguous. This check names the conflict before a start
/// fails on it.
pub(super) fn check_host_subnet() -> DoctorCheck {
    const NAME: &str = "host_subnet";

    match crate::network::bridge::subnet_conflicts() {
        Ok(conflicts) if conflicts.is_empty() => DoctorCheck::ok(
            NAME,
            &format!(
                "{} is assigned to {} alone",
                ipam::SUBNET_CIDR,
                crate::network::bridge::BRIDGE_NAME
            ),
        ),
        Ok(conflicts) => DoctorCheck::warn(
            NAME,
            &format!(
                "{} is also assigned to {}; workspace traffic on that prefix is ambiguous",
                ipam::SUBNET_CIDR,
                conflicts.join(", ")
            ),
        ),
        Err(err) => DoctorCheck::warn(NAME, &format!("host address inventory unknown: {err:#}")),
    }
}

/// Inventory loop devices backing Enclave workspace disk images.
pub(super) fn check_workspace_loop_devices(state_dir: &Path) -> DoctorCheck {
    const NAME: &str = "workspace_loop_devices";

    let expected = match expected_disk_images(state_dir) {
        Ok(expected) => expected,
        Err(err) => {
            return DoctorCheck::warn(
                NAME,
                &format!("failed to read workspace storage ownership: {err:#}"),
            )
        }
    };
    if expected.is_empty() {
        return DoctorCheck::ok(NAME, "no quota-backed workspaces are registered");
    }

    let devices = match list_loop_devices() {
        Ok(devices) => devices,
        Err(err) => return DoctorCheck::warn(NAME, &format!("loop inventory unknown: {err:#}")),
    };
    let leaked = leaked_loop_devices(state_dir, &expected, &devices);
    if leaked.is_empty() {
        return DoctorCheck::ok(
            NAME,
            &format!(
                "{} quota-backed workspace image(s) have no unexpected loop device",
                expected.len()
            ),
        );
    }
    DoctorCheck::warn(
        NAME,
        &format!(
            "{} loop device(s) still back a stopped workspace image: {}",
            leaked.len(),
            leaked.join(", ")
        ),
    )
}

#[derive(Debug, Default, PartialEq, Eq)]
struct VethDiff {
    leaked_veths: Vec<String>,
    missing_veths: Vec<String>,
}

impl VethDiff {
    fn is_clean(&self) -> bool {
        self.leaked_veths.is_empty() && self.missing_veths.is_empty()
    }
}

/// Map every veth name Enclave expects to the workspace that owns it.
fn expected_network_owners(state_dir: &Path) -> Result<BTreeMap<String, String>> {
    crate::registry::with_registry(state_dir, |registry| {
        let mut expected = BTreeMap::new();
        for sandbox in registry.sandboxes.values() {
            for workspace in sandbox.workspaces.values() {
                let Some(ip) = workspace.assigned_ip.as_deref() else {
                    continue;
                };
                let Some(octet) = ipam::parse_host_octet(ip) else {
                    continue;
                };
                let (host, _) = veth::veth_names(octet, &workspace.id);
                expected.insert(host, workspace.id.clone());
            }
        }
        Ok(expected)
    })
}

fn host_enclave_veth_names(net_class_dir: &Path) -> std::io::Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(net_class_dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if veth::is_enclave_veth_name(&name) {
            names.insert(name);
        }
    }
    Ok(names)
}

fn veth_inventory_diff(
    expected: &BTreeMap<String, String>,
    present: &BTreeSet<String>,
) -> VethDiff {
    VethDiff {
        leaked_veths: present
            .iter()
            .filter(|name| !expected.contains_key(*name))
            .cloned()
            .collect(),
        missing_veths: expected
            .iter()
            .filter(|(name, _)| !present.contains(*name))
            .map(|(name, owner)| format!("{name} ({owner})"))
            .collect(),
    }
}

fn leaked_rule_owners(expected: &BTreeSet<String>, rules: &[nat::OwnedRule]) -> Vec<String> {
    let mut leaked = BTreeSet::new();
    for rule in rules {
        if !expected.contains(&rule.owner) {
            leaked.insert(rule.owner.clone());
        }
    }
    leaked.into_iter().collect()
}

/// Map each quota-backed workspace image to whether its runtime is active.
fn expected_disk_images(state_dir: &Path) -> Result<BTreeMap<PathBuf, (String, bool)>> {
    crate::registry::with_registry(state_dir, |registry| {
        let mut expected = BTreeMap::new();
        for sandbox in registry.sandboxes.values() {
            for workspace in sandbox.workspaces.values() {
                if !crate::workspace::workspace_uses_disk_image(workspace) {
                    continue;
                }
                let image = crate::workspace::workspace_disk_image_path(workspace);
                let active = crate::workspace::workspace_runtime_is_active(workspace);
                expected.insert(image, (workspace.id.clone(), active));
            }
        }
        Ok(expected)
    })
}

fn list_loop_devices() -> Result<Vec<(String, PathBuf)>> {
    let output = Command::new("losetup")
        .arg("-a")
        .output()
        .map_err(|err| anyhow::anyhow!("failed to run losetup: {err}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "losetup -a failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(parse_loop_devices(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_loop_devices(output: &str) -> Vec<(String, PathBuf)> {
    output
        .lines()
        .filter_map(|line| {
            let (device, rest) = line.split_once(':')?;
            let device = device.trim();
            if !device.starts_with("/dev/loop") {
                return None;
            }
            // The backing path is the parenthesised group that follows the
            // device numbers; a deleted backing adds a nested marker.
            let start = rest.find('(')?;
            let end = rest.rfind(')')?;
            if end <= start {
                return None;
            }
            let backing = rest[start + 1..end].trim();
            // The kernel appends this marker when the backing file was
            // removed while the loop device stayed attached.
            let backing = backing.strip_suffix(" (deleted)").unwrap_or(backing);
            if backing.is_empty() {
                return None;
            }
            Some((device.to_string(), PathBuf::from(backing)))
        })
        .collect()
}

fn leaked_loop_devices(
    state_dir: &Path,
    expected: &BTreeMap<PathBuf, (String, bool)>,
    devices: &[(String, PathBuf)],
) -> Vec<String> {
    let sandboxes_root = state_dir.join("sandboxes");
    let mut leaked = Vec::new();
    for (device, backing) in devices {
        if !backing.starts_with(&sandboxes_root) {
            continue;
        }
        match expected.get(backing) {
            Some((owner, true)) => {
                let _ = owner;
            }
            Some((owner, false)) => leaked.push(format!("{device} -> {owner}")),
            None => leaked.push(format!("{device} -> {}", backing.display())),
        }
    }
    leaked
}

#[cfg(test)]
#[path = "../../tests/src/doctor/network.rs"]
mod tests;
