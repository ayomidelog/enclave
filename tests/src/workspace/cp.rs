use super::path::{
    destination_plan, source_name, validate_direction_paths, validate_host_destination,
    validate_host_source, Direction,
};
use super::stream::{
    extract_workspace_archive, open_host_directory, validate_archive_entry_type,
    validate_archive_path, wait_child_output, workspace_tar_command, write_host_directory_archive,
    ChildGuard, HostStagingDirectory,
};

#[test]
fn source_name_rejects_root_and_parent_entries() {
    assert!(source_name("/").is_err());
    assert!(source_name("/tmp/..").is_err());
    assert_eq!(source_name("/tmp/source.txt").unwrap(), "source.txt");
}

#[test]
fn host_directory_archive_validates_entries_and_reports_stats() {
    let root = std::env::temp_dir().join(format!(
        "enclave-cp-archive-walk-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(root.join("nested/file.txt"), b"payload").unwrap();
    let mut archive = Vec::new();
    let (bytes, files) =
        write_host_directory_archive(root.to_str().unwrap(), "project", &mut archive).unwrap();
    assert_eq!(bytes, 7);
    assert_eq!(files, 1);
    let mut reader = tar::Archive::new(archive.as_slice());
    let entries = reader.entries().unwrap().count();
    assert_eq!(entries, 3);
    let _ = std::fs::remove_dir_all(root);
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

#[test]
fn workspace_tar_command_includes_executable_before_flags() {
    assert_eq!(
        workspace_tar_command(vec!["-C".into(), "/home".into(), "-cf".into(), "-".into()]),
        vec!["tar", "-C", "/home", "-cf", "-"]
    );
}

#[test]
fn gzip_tar_commands_use_compressed_archive_flags() {
    assert_eq!(
        super::stream::tar_create_args("/home/project", "project", true),
        vec!["-C", "/home", "-czf", "-", "--", "project"]
    );
    assert_eq!(
        super::stream::tar_extract_args_at("/home/.stage", true),
        vec!["-C", "/home/.stage", "-xzpf", "-", "-o", "--"]
    );
}

#[test]
fn host_source_rejects_fifo() {
    let root = std::env::temp_dir().join(format!("enclave-cp-fifo-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let fifo = root.join("source.fifo");
    let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);

    let error = validate_host_source(&fifo.to_string_lossy()).unwrap_err();
    assert!(error.to_string().contains("unsupported host source type"));
    std::fs::remove_file(&fifo).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn archive_validation_rejects_unsafe_and_unexpected_entries() {
    assert!(validate_archive_path(std::path::Path::new("../escape"), "source").is_err());
    assert!(validate_archive_path(std::path::Path::new("/absolute"), "source").is_err());
    assert!(validate_archive_path(std::path::Path::new("other/file"), "source").is_err());
    assert!(validate_archive_entry_type(tar::EntryType::new(b'1')).is_err());
    assert!(validate_archive_entry_type(tar::EntryType::new(b'3')).is_err());
}

#[test]
fn hostile_archive_is_rejected_and_staging_is_removed() {
    let root = std::env::temp_dir().join(format!(
        "enclave-cp-hostile-archive-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let mut archive_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut archive_data);
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o600);
        header.set_cksum();
        builder
            .append_data(&mut header, "unexpected.txt", &b"oops"[..])
            .unwrap();
        builder.finish().unwrap();
    }

    let stage = HostStagingDirectory::create(&root).unwrap();
    let stage_path = stage.path().to_path_buf();
    let error = extract_workspace_archive(&archive_data[..], &stage, "source").unwrap_err();
    assert!(error.to_string().contains("unexpected entry"));
    drop(stage);
    assert!(!stage_path.exists());
    assert!(std::fs::read_dir(&root).unwrap().next().is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn validated_archive_commits_once_without_overwriting_destination() {
    let root = std::env::temp_dir().join(format!(
        "enclave-cp-commit-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let mut archive_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut archive_data);
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o600);
        header.set_cksum();
        builder
            .append_data(&mut header, "source.txt", &b"safe"[..])
            .unwrap();
        builder.finish().unwrap();
    }

    let stage = HostStagingDirectory::create(&root).unwrap();
    assert_eq!(
        extract_workspace_archive(&archive_data[..], &stage, "source.txt").unwrap(),
        4
    );
    stage.commit("source.txt", "destination.txt").unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("destination.txt")).unwrap(),
        "safe"
    );

    std::fs::write(root.join("existing.txt"), "keep").unwrap();
    let stage = HostStagingDirectory::create(&root).unwrap();
    extract_workspace_archive(&archive_data[..], &stage, "source.txt").unwrap();
    assert!(stage.commit("source.txt", "existing.txt").is_err());
    assert_eq!(
        std::fs::read_to_string(root.join("existing.txt")).unwrap(),
        "keep"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn secure_host_parent_open_rejects_symlink() {
    let root = std::env::temp_dir().join(format!("enclave-cp-parent-test-{}", std::process::id()));
    let real = root.join("real");
    let link = root.join("link");
    std::fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    assert!(open_host_directory(&link).is_err());
    std::fs::remove_file(link).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
