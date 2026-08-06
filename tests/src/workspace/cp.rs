use super::path::{
    destination_plan, source_name, validate_direction_paths, validate_host_destination, Direction,
};
use super::stream::{wait_child_output, ChildGuard};

#[test]
fn source_name_rejects_root_and_parent_entries() {
    assert!(source_name("/").is_err());
    assert!(source_name("/tmp/..").is_err());
    assert_eq!(source_name("/tmp/source.txt").unwrap(), "source.txt");
}

#[test]
fn destination_directory_keeps_archive_name() {
    let plan = destination_plan("/home/project", "source.txt", true).unwrap();
    assert_eq!(plan.parent, std::path::Path::new("/home/project"));
    assert!(plan.rename_to.is_none());
}

#[test]
fn destination_directory_semantics_match_cp_entry_copy() {
    let plan = destination_plan("/home/existing", "project", true).unwrap();
    assert_eq!(
        plan.extracted_path("project"),
        std::path::Path::new("/home/existing/project")
    );
    assert_eq!(
        plan.final_path("project"),
        std::path::Path::new("/home/existing/project")
    );

    let plan = destination_plan("/home/project/", "project", false).unwrap();
    assert_eq!(
        plan.extracted_path("project"),
        std::path::Path::new("/home/project")
    );
    assert_eq!(
        plan.final_path("project"),
        std::path::Path::new("/home/project")
    );
}

#[test]
fn destination_file_renames_archive_root() {
    let plan = destination_plan("/tmp/output.txt", "source.txt", false).unwrap();
    assert_eq!(plan.parent, std::path::Path::new("/tmp"));
    assert_eq!(plan.rename_to.as_deref(), Some("output.txt"));
}

#[test]
fn direction_validation_requires_workspace_side() {
    assert!(
        validate_direction_paths("./source", "./destination", Direction::HostToWorkspace).is_err()
    );
    assert!(validate_direction_paths(
        "/home/source",
        "/tmp/destination",
        Direction::WorkspaceToHost
    )
    .is_ok());
}

#[test]
fn host_destination_rejects_symlinked_components() {
    let root = std::env::temp_dir().join(format!("enclave-cp-path-test-{}", std::process::id()));
    let real = root.join("real");
    let link = root.join("link");
    std::fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let error = validate_host_destination(&link.join("output").to_string_lossy()).unwrap_err();
    assert!(error.to_string().contains("symlinked host destination"));
    std::fs::remove_file(link).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn disconnected_client_cancels_child_process() {
    let (server, client) = std::os::unix::net::UnixStream::pair().unwrap();
    drop(client);
    let child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let mut child = ChildGuard::new(child);
    let error = wait_child_output(&mut child, Some(&server)).unwrap_err();
    assert!(error.to_string().contains("client disconnected"));
}
