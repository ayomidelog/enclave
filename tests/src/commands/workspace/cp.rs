use super::super::super::super::commands::workspace::cp::{
    parse_transfer_path, parse_transfer_paths, PathSide,
};

#[test]
fn parses_workspace_prefix_as_a_workspace_path() {
    let path = parse_transfer_path("ws:/home/file").unwrap();
    assert_eq!(path.side, PathSide::Workspace);
    assert_eq!(path.path, "/home/file");
}

#[test]
fn rejects_both_host_paths() {
    let error = parse_transfer_paths("./source", "./destination").unwrap_err();
    assert!(error.to_string().contains("exactly one workspace path"));
}

#[test]
fn rejects_both_workspace_paths() {
    let error = parse_transfer_paths("ws:/source", "ws:/destination").unwrap_err();
    assert!(error.to_string().contains("exactly one workspace path"));
}

#[test]
fn rejects_workspace_parent_traversal() {
    let error = parse_transfer_paths("./source", "ws:/home/../tmp").unwrap_err();
    assert!(error.to_string().contains("cannot contain '..'"));
}

#[test]
fn rejects_malformed_workspace_prefix() {
    let error = parse_transfer_paths("ws:home/file", "./destination").unwrap_err();
    assert!(error.to_string().contains("must use the 'ws:/' prefix"));
}
