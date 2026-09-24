use std::fs;
use std::path::Path;

use crate::registry::with_registry;

use super::DoctorCheck;

pub(crate) fn check_registry_consistency(state_dir: &Path) -> DoctorCheck {
    let name = "registry_consistency";
    let registry_path = state_dir.join("registry.json");
    if !registry_path.exists() {
        return DoctorCheck::warn(name, "registry.json not found");
    }

    match with_registry(state_dir, |registry| {
        let mut issues = Vec::new();
        let sandboxes_dir = state_dir.join("sandboxes");

        for (sandbox_id, sandbox) in &registry.sandboxes {
            let sandbox_path = std::path::PathBuf::from(&sandbox.metadata.sandbox_path);
            if !sandbox_path.exists() {
                issues.push(format!(
                    "sandbox '{}' registered but directory missing",
                    sandbox_id
                ));
            }
            for (workspace_id, workspace) in &sandbox.workspaces {
                let ws_path = std::path::PathBuf::from(&workspace.workspace_path);
                if !ws_path.exists() {
                    issues.push(format!(
                        "workspace '{}' in sandbox '{}' registered but directory missing",
                        workspace_id, sandbox_id
                    ));
                }
            }
        }

        if sandboxes_dir.exists() {
            if let Ok(entries) = fs::read_dir(&sandboxes_dir) {
                for entry in entries.flatten() {
                    let dir_name = entry.file_name().to_string_lossy().to_string();
                    if dir_name == "rootfs-cache" {
                        continue;
                    }
                    if entry.path().is_dir()
                        && !registry.sandboxes.contains_key(&dir_name)
                        && entry.path().join("sandbox.json").exists()
                    {
                        issues.push(format!(
                            "sandbox directory '{}' exists on disk but not in registry",
                            dir_name
                        ));
                    }
                }
            }
        }

        Ok(issues)
    }) {
        Ok(issues) => {
            if issues.is_empty() {
                DoctorCheck::ok(name, "registry is consistent with disk state")
            } else {
                DoctorCheck::warn(
                    name,
                    &format!("{} issue(s) found: {}", issues.len(), issues.join("; ")),
                )
            }
        }
        Err(err) => DoctorCheck::warn(name, &format!("failed to read registry: {err:#}")),
    }
}
