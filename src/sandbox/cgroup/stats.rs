//! Reading a cgroup's current usage and limits.

use std::fs;
use std::path::Path;

use anyhow::Result;

#[derive(Debug, Clone, Default)]
pub struct CgroupStats {
    pub memory_current_bytes: Option<u64>,
    pub memory_max_bytes: Option<String>,
    pub pids_current: Option<u64>,
    pub pids_max: Option<String>,
    pub cpu_max: Option<String>,
}

impl std::fmt::Display for CgroupStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "memory={}/{}, pids={}/{}, cpu_max={}",
            self.memory_current_bytes
                .map_or("unknown".to_string(), |v| format!("{}B", v)),
            self.memory_max_bytes.as_deref().unwrap_or("max"),
            self.pids_current
                .map_or("unknown".to_string(), |v| v.to_string()),
            self.pids_max.as_deref().unwrap_or("max"),
            self.cpu_max.as_deref().unwrap_or("max"),
        )
    }
}

pub fn read_cgroup_stats(cgroup_path: &Path) -> Result<CgroupStats> {
    let memory_current = read_cgroup_u64(&cgroup_path.join("memory.current"));
    let memory_max = read_cgroup_string(&cgroup_path.join("memory.max"));
    let pids_current = read_cgroup_u64(&cgroup_path.join("pids.current"));
    let pids_max = read_cgroup_string(&cgroup_path.join("pids.max"));
    let cpu_max = read_cgroup_string(&cgroup_path.join("cpu.max"));

    Ok(CgroupStats {
        memory_current_bytes: memory_current,
        memory_max_bytes: memory_max,
        pids_current,
        pids_max,
        cpu_max,
    })
}

/// A missing or unparseable counter is reported as unknown rather than as an
/// error: a cgroup can disappear between listing it and reading it.
fn read_cgroup_u64(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse::<u64>().ok()
}

fn read_cgroup_string(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}
