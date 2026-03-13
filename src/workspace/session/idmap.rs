use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use anyhow::{Context, Result};

use super::userns::UserNamespacePlan;

#[derive(Debug, Clone, PartialEq, Eq)]
struct IdMapEntry {
    id_type: char,
    mount_start: u32,
    host_start: u32,
    count: u32,
}

pub fn workspace_bind_mount_idmap_option(
    source: &Path,
    userns: &UserNamespacePlan,
) -> Result<Option<String>> {
    let metadata = fs::metadata(source)
        .with_context(|| format!("failed to stat workspace source {}", source.display()))?;
    let mut entries = Vec::new();
    entries.extend(build_idmap_entries(
        'u',
        userns.uid_map.outer_start,
        userns.uid_map.count,
        metadata.uid(),
    ));
    entries.extend(build_idmap_entries(
        'g',
        userns.gid_map.outer_start,
        userns.gid_map.count,
        metadata.gid(),
    ));
    if entries
        .iter()
        .all(|entry| entry.mount_start == entry.host_start)
    {
        return Ok(None);
    }
    Ok(Some(render_idmap_entries(&entries)))
}

fn build_idmap_entries(
    id_type: char,
    mount_root_id: u32,
    range_count: u32,
    host_owner_id: u32,
) -> Vec<IdMapEntry> {
    let mut entries = vec![IdMapEntry {
        id_type,
        mount_start: mount_root_id,
        host_start: host_owner_id,
        count: 1,
    }];

    if range_count <= 1 {
        return entries;
    }

    let passthrough_start = mount_root_id.saturating_add(1);
    let passthrough_end = mount_root_id.saturating_add(range_count).saturating_sub(1);

    if host_owner_id < passthrough_start || host_owner_id > passthrough_end {
        entries.push(IdMapEntry {
            id_type,
            mount_start: passthrough_start,
            host_start: passthrough_start,
            count: range_count - 1,
        });
        return entries;
    }

    if passthrough_start < host_owner_id {
        entries.push(IdMapEntry {
            id_type,
            mount_start: passthrough_start,
            host_start: passthrough_start,
            count: host_owner_id - passthrough_start,
        });
    }
    if host_owner_id < passthrough_end {
        let next = host_owner_id.saturating_add(1);
        entries.push(IdMapEntry {
            id_type,
            mount_start: next,
            host_start: next,
            count: passthrough_end - host_owner_id,
        });
    }

    entries
}

fn render_idmap_entries(entries: &[IdMapEntry]) -> String {
    entries
        .iter()
        .map(|entry| {
            format!(
                "{}:{}:{}:{}",
                entry.id_type, entry.mount_start, entry.host_start, entry.count
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[path = "../../../tests/src/workspace/session/idmap.rs"]
mod tests;
