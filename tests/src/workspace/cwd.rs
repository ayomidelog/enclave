use super::sanitize_workspace_cwd;

#[test]
fn sanitize_cwd_allows_workspace_paths() {
    assert_eq!(sanitize_workspace_cwd("/home"), "/home");
    assert_eq!(sanitize_workspace_cwd("/home/src"), "/home/src");
}

#[test]
fn sanitize_cwd_rejects_outside_paths() {
    assert_eq!(sanitize_workspace_cwd("/etc"), "/home");
    assert_eq!(sanitize_workspace_cwd("/tmp"), "/home");
    assert_eq!(sanitize_workspace_cwd("/root"), "/home");
    assert_eq!(sanitize_workspace_cwd("/"), "/home");
    assert_eq!(sanitize_workspace_cwd("/projects2"), "/home");
    assert_eq!(sanitize_workspace_cwd("/home-old"), "/home");
}

#[test]
fn sanitize_cwd_rejects_parent_traversal() {
    assert_eq!(sanitize_workspace_cwd("/home/../../etc"), "/home");
}

#[test]
fn sanitize_cwd_rejects_control_characters() {
    assert_eq!(sanitize_workspace_cwd("/home/\n/evil"), "/home");
    assert_eq!(sanitize_workspace_cwd("/home/\t/evil"), "/home");
}

#[test]
fn sanitize_cwd_normalizes_redundant_segments() {
    assert_eq!(sanitize_workspace_cwd("/home//src"), "/home/src");
    assert_eq!(sanitize_workspace_cwd("/home/./src"), "/home/src");
    assert_eq!(sanitize_workspace_cwd("/home/"), "/home");
}
