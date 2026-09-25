//! Tests for the filesystem helpers.
//!
//! The fixtures are here because the submodules share them: a directory to write into,
//! a fake sysfs entry for the loop devices, and a mountinfo line to parse.

use std::path::PathBuf;

use super::*;

fn loopback_fixture_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("enclave-loopback-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_backing(sys_block: &std::path::Path, name: &str, backing: &str) {
    let loop_dir = sys_block.join(name).join("loop");
    fs::create_dir_all(&loop_dir).unwrap();
    fs::write(loop_dir.join("backing_file"), backing).unwrap();
}

/// Parse one mountinfo line into the entry the ownership rules read.
fn entry(line: &str) -> MountInfoEntry {
    MountInfoSnapshot::parse(line)
        .at_or_below_entries(Path::new("/"))
        .into_iter()
        .next()
        .cloned()
        .expect("a well-formed mountinfo line should parse")
}

fn marker_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "enclave-creation-marker-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create marker dir");
    dir
}

mod copy;
mod creation;
mod file;
mod loopback;
mod mountinfo;
