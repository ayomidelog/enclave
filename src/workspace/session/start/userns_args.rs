//! The user-namespace arguments the session launcher is given.
//!
//! The mapping is decided in the parent because only the parent knows whether
//! the host allows a subordinate range; the launcher applies what it is told.

use super::*;

pub(crate) fn setgroups_args(userns: &userns::UserNamespacePlan) -> Vec<&'static str> {
    if userns.gid_map.count > 1 {
        vec!["--deny-setgroups"]
    } else {
        Vec::new()
    }
}

pub(crate) fn launch_userns_args(userns: &UserNamespaceMode) -> Vec<String> {
    match userns {
        UserNamespaceMode::Enabled(plan) => {
            let mut args = vec![
                "--enable-userns".to_string(),
                "--uid-inner".to_string(),
                plan.uid_map.inner_start.to_string(),
                "--uid-outer".to_string(),
                plan.uid_map.outer_start.to_string(),
                "--uid-count".to_string(),
                plan.uid_map.count.to_string(),
                "--gid-inner".to_string(),
                plan.gid_map.inner_start.to_string(),
                "--gid-outer".to_string(),
                plan.gid_map.outer_start.to_string(),
                "--gid-count".to_string(),
                plan.gid_map.count.to_string(),
            ];
            args.extend(setgroups_args(plan).into_iter().map(String::from));
            args
        }
        UserNamespaceMode::Disabled => vec![
            "--uid-inner".to_string(),
            "0".to_string(),
            "--uid-outer".to_string(),
            "0".to_string(),
            "--uid-count".to_string(),
            "1".to_string(),
            "--gid-inner".to_string(),
            "0".to_string(),
            "--gid-outer".to_string(),
            "0".to_string(),
            "--gid-count".to_string(),
            "1".to_string(),
        ],
    }
}
