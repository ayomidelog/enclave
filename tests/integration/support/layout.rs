//! Finding the directory a sandbox or workspace was given.
//!
//! The daemon names a directory from the id it assigned, so a test that knows
//! only the name the operator used has to look it up the same way.

use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn sandbox_dir(state_dir: &Path, name: &str) -> PathBuf {
    let root = state_dir.join("sandboxes");
    let mut matches = fs::read_dir(&root)
        .expect("read the sandboxes directory")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|file| file.to_str())
                .is_some_and(|file| file.starts_with(&format!("{name}-")))
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no sandbox directory for {name} under {}", root.display()))
}

pub(crate) fn workspace_dir(sandbox: &Path, name: &str) -> PathBuf {
    let root = sandbox.join("workspaces");
    let mut matches = fs::read_dir(&root)
        .expect("read the workspaces directory")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|file| file.to_str())
                .is_some_and(|file| file.starts_with(&format!("{name}-")))
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no workspace directory for {name} under {}", root.display()))
}
