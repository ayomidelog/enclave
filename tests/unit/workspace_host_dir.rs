use std::fs;

use enclave::enclavefile::resolve_workspace_host_dir;
use uuid::Uuid;

fn temp_case_dir(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "enclave-workspace-host-dir-{name}-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ))
}

#[test]
fn resolve_workspace_host_dir_resolves_relative_paths_from_enclavefile_parent() {
    let root = temp_case_dir("relative");
    let project = root.join("project");
    fs::create_dir_all(&project).expect("create project dir");
    let enclavefile = root.join("Enclavefile");
    fs::write(&enclavefile, "").expect("write Enclavefile");

    let resolved = resolve_workspace_host_dir(&enclavefile, "./project", "workspace_dir")
        .expect("resolve dir");

    assert_eq!(
        resolved,
        project
            .canonicalize()
            .expect("canonicalize project dir")
            .to_string_lossy()
            .to_string()
    );

    fs::remove_dir_all(root).expect("cleanup temp root");
}

#[test]
fn resolve_workspace_host_dir_accepts_absolute_paths() {
    let root = temp_case_dir("absolute");
    fs::create_dir_all(&root).expect("create root");
    let enclavefile = root.join("Enclavefile");
    fs::write(&enclavefile, "").expect("write Enclavefile");

    let resolved = resolve_workspace_host_dir(
        &enclavefile,
        root.to_string_lossy().as_ref(),
        "workspace_dir",
    )
    .expect("resolve dir");

    assert_eq!(
        resolved,
        root.canonicalize()
            .expect("canonicalize root")
            .to_string_lossy()
            .to_string()
    );

    fs::remove_dir_all(root).expect("cleanup temp root");
}

#[test]
fn resolve_workspace_host_dir_rejects_missing_paths() {
    let root = temp_case_dir("missing");
    fs::create_dir_all(&root).expect("create root");
    let enclavefile = root.join("Enclavefile");
    fs::write(&enclavefile, "").expect("write Enclavefile");

    let err = resolve_workspace_host_dir(&enclavefile, "./missing", "workspace_dir")
        .expect_err("missing path should fail");
    assert!(err
        .to_string()
        .contains("ensure the directory exists and is readable"));

    fs::remove_dir_all(root).expect("cleanup temp root");
}

#[test]
fn resolve_workspace_host_dir_rejects_non_directories() {
    let root = temp_case_dir("file");
    fs::create_dir_all(&root).expect("create root");
    let file_path = root.join("project.txt");
    fs::write(&file_path, "hello").expect("write file");
    let enclavefile = root.join("Enclavefile");
    fs::write(&enclavefile, "").expect("write Enclavefile");

    let err = resolve_workspace_host_dir(&enclavefile, "./project.txt", "workspace_dir")
        .expect_err("file path should fail");
    assert!(err.to_string().contains("existing directory"));

    fs::remove_dir_all(root).expect("cleanup temp root");
}
