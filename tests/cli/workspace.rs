//! Creating and entering a workspace, and the arguments that are refused.

use super::*;

#[test]
fn workspace_enter_default_cwd_is_home() {
    let cli = Cli::parse_from(["enclave", "workspace", "enter", "sb", "ws"]);
    let Commands::Workspace { command } = cli.command else {
        panic!("expected workspace command");
    };
    let WorkspaceCommands::Enter(args) = command else {
        panic!("expected workspace enter command");
    };
    assert_eq!(args.cwd, "/home");
}

#[test]
fn workspace_exec_default_cwd_is_home() {
    let cli = Cli::parse_from(["enclave", "workspace", "exec", "sb", "ws", "pwd"]);
    let Commands::Workspace { command } = cli.command else {
        panic!("expected workspace command");
    };
    let WorkspaceCommands::Exec(args) = command else {
        panic!("expected workspace exec command");
    };
    assert_eq!(args.cwd, "/home");
}

#[test]
fn workspace_exec_rejects_empty_command_argument() {
    let parsed = Cli::try_parse_from(["enclave", "workspace", "exec", "sb", "ws", ""]);
    assert!(parsed.is_err());
}

#[test]
fn workspace_enter_rejects_shell_arguments() {
    let parsed = Cli::try_parse_from([
        "enclave",
        "workspace",
        "enter",
        "sb",
        "ws",
        "--shell",
        "bash -x",
    ]);
    assert!(parsed.is_err());
}

#[test]
fn workspace_create_rejects_unsafe_selector_name() {
    let parsed = Cli::try_parse_from(["enclave", "workspace", "create", "../sb", "ws"]);
    assert!(parsed.is_err());
}

#[test]
fn workspace_create_parses_cpu_percent() {
    let cli = Cli::parse_from([
        "enclave",
        "workspace",
        "create",
        "sb",
        "ws",
        "--cpu-percent",
        "25",
        "--memory-mb",
        "2048",
    ]);
    let Commands::Workspace { command } = cli.command else {
        panic!("expected workspace command");
    };
    let WorkspaceCommands::Create(args) = command else {
        panic!("expected workspace create command");
    };
    assert_eq!(args.cpu_percent, Some(25.0));
    assert_eq!(args.memory_mb, Some(2048));
}
