use clap::Parser;
use enclave::cli::{
    Cli, Commands, RootfsCommands, SnapshotCommands, WorkspaceCommands, WorkspacePortCommands,
};

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

#[test]
fn sandbox_create_parses_limit_flags() {
    let cli = Cli::parse_from([
        "enclave",
        "create",
        "sb",
        "--cpu-percent",
        "40",
        "--memory-mb",
        "8192",
        "--max-procs",
        "1024",
    ]);
    let Commands::Create(args) = cli.command else {
        panic!("expected create command");
    };
    assert_eq!(args.cpu_percent, Some(40.0));
    assert_eq!(args.memory_mb, Some(8192));
    assert_eq!(args.max_procs, Some(1024));
}

#[test]
fn rootfs_export_parses_suite_and_output() {
    let cli = Cli::parse_from([
        "enclave",
        "rootfs",
        "export",
        "--suite",
        "bookworm",
        "--output",
        "/tmp/bookworm-rootfs.tar.gz",
    ]);
    let Commands::Rootfs { command } = cli.command else {
        panic!("expected rootfs command");
    };
    let RootfsCommands::Export(args) = command else {
        panic!("expected rootfs export command");
    };
    assert_eq!(args.suite.as_deref(), Some("bookworm"));
    assert!(!args.base);
    assert_eq!(
        args.output,
        std::path::PathBuf::from("/tmp/bookworm-rootfs.tar.gz")
    );
}

#[test]
fn rootfs_fetch_parses_base_url_and_replace() {
    let cli = Cli::parse_from([
        "enclave",
        "rootfs",
        "fetch",
        "--base",
        "--replace",
        "https://example.com/rootfs.tar.gz",
    ]);
    let Commands::Rootfs { command } = cli.command else {
        panic!("expected rootfs command");
    };
    let RootfsCommands::Fetch(args) = command else {
        panic!("expected rootfs fetch command");
    };
    assert!(args.base);
    assert!(args.replace);
    assert_eq!(args.url, "https://example.com/rootfs.tar.gz");
}

#[test]
fn snapshot_export_parses_output() {
    let cli = Cli::parse_from([
        "enclave",
        "snapshot",
        "export",
        "sb",
        "ws",
        "snap1",
        "--output",
        "/tmp/snap1.tar.gz",
    ]);
    let Commands::Snapshot { command } = cli.command else {
        panic!("expected snapshot command");
    };
    let SnapshotCommands::Export(args) = command else {
        panic!("expected snapshot export command");
    };
    assert_eq!(args.sandbox, "sb");
    assert_eq!(args.workspace, "ws");
    assert_eq!(args.snapshot, "snap1");
    assert_eq!(args.output, std::path::PathBuf::from("/tmp/snap1.tar.gz"));
}

#[test]
fn snapshot_import_parses_name_and_replace() {
    let cli = Cli::parse_from([
        "enclave",
        "snapshot",
        "import",
        "sb",
        "ws",
        "--name",
        "snap2",
        "--replace",
        "/tmp/snap1.tar.gz",
    ]);
    let Commands::Snapshot { command } = cli.command else {
        panic!("expected snapshot command");
    };
    let SnapshotCommands::Import(args) = command else {
        panic!("expected snapshot import command");
    };
    assert_eq!(args.sandbox, "sb");
    assert_eq!(args.workspace, "ws");
    assert_eq!(args.name.as_deref(), Some("snap2"));
    assert!(args.replace);
    assert_eq!(args.archive, std::path::PathBuf::from("/tmp/snap1.tar.gz"));
}
