//! Asking a workspace what it is doing: logs, stats, and published ports.

use super::*;

#[test]
fn workspace_logs_accepts_project_target_and_follow_flag() {
    let cli = Cli::parse_from(["enclave", "workspace", "logs", "api", "--follow"]);
    let Commands::Workspace { command } = cli.command else {
        panic!("expected workspace command");
    };
    let WorkspaceCommands::Logs(args) = command else {
        panic!("expected workspace logs command");
    };
    assert_eq!(args.target, "api");
    assert!(args.workspace.is_none());
    assert!(args.follow);
}

#[test]
fn workspace_stats_accepts_project_target() {
    let cli = Cli::parse_from(["enclave", "workspace", "stats", "api"]);
    let Commands::Workspace { command } = cli.command else {
        panic!("expected workspace command");
    };
    let WorkspaceCommands::Stats(args) = command else {
        panic!("expected workspace stats command");
    };
    assert_eq!(args.target, "api");
    assert!(args.workspace.is_none());
}

#[test]
fn top_level_stats_command_parses() {
    let cli = Cli::parse_from(["enclave", "stats"]);
    assert!(matches!(cli.command, Commands::Stats));
}

#[test]
fn workspace_port_publish_parses() {
    let cli = Cli::parse_from([
        "enclave",
        "workspace",
        "port",
        "publish",
        "sb",
        "ws",
        "127.0.0.1:3001:3000",
    ]);
    let Commands::Workspace { command } = cli.command else {
        panic!("expected workspace command");
    };
    let WorkspaceCommands::Port { command } = command else {
        panic!("expected workspace port command");
    };
    let WorkspacePortCommands::Publish(args) = command else {
        panic!("expected workspace port publish command");
    };
    assert_eq!(args.sandbox, "sb");
    assert_eq!(args.workspace, "ws");
    assert_eq!(args.spec, "127.0.0.1:3001:3000");
}
