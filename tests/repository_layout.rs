use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn collect_files(root: &Path, extension: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some(extension) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[test]
fn scripts_directory_contains_only_release_scripts() {
    let scripts_dir = repo_root().join("scripts");
    let mut names = fs::read_dir(scripts_dir)
        .expect("read scripts dir")
        .map(|entry| {
            entry
                .expect("script entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        vec![
            "install-remote.sh".to_string(),
            "install.sh".to_string(),
            "uninstall.sh".to_string(),
            "update.sh".to_string()
        ]
    );
}

#[test]
fn install_remote_targets_release_repository() {
    let installer = fs::read_to_string(repo_root().join("scripts/install-remote.sh"))
        .expect("read install-remote.sh");
    assert!(installer.contains("https://github.com/ayomidelog/enclave.git"));
    assert!(!installer.contains("Enclave-Beta"));
    assert!(installer.contains("cargo build --release --manifest-path"));
    assert!(!installer.contains("| grep"));
}

#[test]
fn source_tree_has_no_inline_test_bodies() {
    for path in collect_files(&repo_root().join("src"), "rs") {
        let text = fs::read_to_string(&path).expect("read source file");
        assert!(
            !text.contains("mod tests {"),
            "inline test module body found in {}",
            path.display()
        );
        assert!(
            !text.contains("#[test]"),
            "inline test function found in {}",
            path.display()
        );
    }
}

#[test]
fn rust_and_script_files_have_no_unresolved_todo_markers() {
    let roots = [
        (repo_root().join("src"), "rs"),
        (repo_root().join("tests"), "rs"),
        (repo_root().join("scripts"), "sh"),
        (repo_root().join(".github/workflows"), "yml"),
    ];
    for (root, extension) in roots {
        for path in collect_files(&root, extension) {
            let text = fs::read_to_string(&path).expect("read file");
            for (line_number, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                let is_comment = if extension == "rs" {
                    trimmed.starts_with("//")
                        || trimmed.starts_with("/*")
                        || trimmed.starts_with("*/")
                } else {
                    trimmed.starts_with('#') && !trimmed.starts_with("#!")
                };
                let has_unresolved_marker =
                    trimmed.contains("TODO") || trimmed.contains("FIXME") || trimmed.contains("XXX");
                assert!(
                    !(is_comment && has_unresolved_marker),
                    "unresolved marker left in {}:{}",
                    path.display(),
                    line_number + 1
                );
            }
        }
    }
}

#[test]
fn release_workflow_runs_verification_before_release() {
    let workflow = fs::read_to_string(repo_root().join(".github/workflows/release.yml"))
        .expect("read release workflow");
    assert!(workflow.contains("workflow_dispatch:"));
    assert!(workflow.contains("cargo fmt --all -- --check"));
    assert!(workflow.contains("cargo check --all-targets"));
    assert!(workflow.contains("cargo clippy --all-targets -- -D warnings"));
    assert!(workflow.contains("cargo test --all-targets -- --skip integration:: --skip stress::"));
    assert!(workflow.contains("cargo build --release --locked"));
    assert!(workflow.contains("softprops/action-gh-release@v2"));
}
