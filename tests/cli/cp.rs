//! Copying between the host and a workspace.

use super::*;

#[test]
fn workspace_cp_parses_host_to_workspace_paths() {
    let cli = Cli::parse_from([
        "enclave",
        "workspace",
        "cp",
        "sb",
        "ws",
        "./source.txt",
        "ws:/home/source.txt",
    ]);
    let Commands::Workspace { command } = cli.command else {
        panic!("expected workspace command");
    };
    let WorkspaceCommands::Cp(args) = command else {
        panic!("expected workspace cp command");
    };
    assert_eq!(args.sandbox, "sb");
    assert_eq!(args.workspace, "ws");
    assert_eq!(args.src, "./source.txt");
    assert_eq!(args.dst, "ws:/home/source.txt");
}

#[test]
fn workspace_cp_requires_two_paths() {
    assert!(Cli::try_parse_from(["enclave", "workspace", "cp", "sb", "ws", "source"]).is_err());
}

#[test]
fn workspace_cp_accepts_reverse_and_directory_examples() {
    for arguments in [
        ["ws:/home/output.txt", "./output.txt"],
        ["./project/", "ws:/home/project/"],
    ] {
        assert!(Cli::try_parse_from([
            "enclave",
            "workspace",
            "cp",
            "mybox",
            "agent1",
            arguments[0],
            arguments[1],
        ])
        .is_ok());
    }
}
