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
                    continue;
                }
                // The registry and the per-directory copy are two records of the
                // same workspace, and a lifecycle step writes the file before it
                // commits the registry. When they disagree, the copies have
                // diverged and repair would adopt the file, so the operator needs
                // to see it before that happens rather than after.
                let metadata_path = ws_path.join("workspace.json");
                let Ok(raw) = fs::read_to_string(&metadata_path) else {
                    continue;
                };
                let Ok(on_disk) = serde_json::from_str::<crate::workspace::WorkspaceMetadata>(&raw)
                else {
                    issues.push(format!(
                        "workspace '{}' in sandbox '{}' has unreadable metadata at {}",
                        workspace_id,
                        sandbox_id,
                        metadata_path.display()
                    ));
                    continue;
                };
                let differences = crate::registry::workspace_state_differences(workspace, &on_disk);
                if !differences.is_empty() {
                    issues.push(format!(
                        "workspace '{}' in sandbox '{}' disagrees with its on-disk metadata ({})",
                        workspace_id,
                        sandbox_id,
                        differences.join(", ")
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
