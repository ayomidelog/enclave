use std::path::{Component, Path};

pub(crate) fn sanitize_workspace_cwd(cwd: &str) -> String {
    if cwd.is_empty() || cwd.chars().any(char::is_control) {
        return "/home".to_string();
    }

    let mut normalized_parts = Vec::new();
    for component in Path::new(cwd).components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => return "/home".to_string(),
            Component::Normal(part) => normalized_parts.push(part.to_string_lossy().to_string()),
            Component::Prefix(_) => return "/home".to_string(),
        }
    }

    if normalized_parts.first().map(|part| part.as_str()) != Some("home") {
        return "/home".to_string();
    }

    if normalized_parts.len() == 1 {
        "/home".to_string()
    } else {
        format!("/{}", normalized_parts.join("/"))
    }
}

#[cfg(test)]
#[path = "../../tests/src/workspace/cwd.rs"]
mod tests;
