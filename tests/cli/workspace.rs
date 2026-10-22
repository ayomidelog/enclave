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
fn workspace_exec_background_flags_parse() {
    let cli = Cli::try_parse_from([
        "enclave",
        "workspace",
        "exec",
        "box",
        "api",
        "--background",
        "--ttl-secs",
        "30",
        "--keep-running",
        "--",
        "sleep",
        "60",
    ])
    .expect("background job flags should parse");
    let Commands::Workspace {
        command: WorkspaceCommands::Exec(args),
    } = cli.command
    else {
        panic!("expected workspace exec");
    };
    assert!(args.background);
    assert!(args.keep_running);
    assert_eq!(args.ttl_secs, Some(30));
}

#[test]
fn background_job_policy_requires_background_mode() {
    for flag in ["--keep-running", "--ttl-secs"] {
        let mut args = vec!["enclave", "workspace", "exec", "box", "api", flag];
        if flag == "--ttl-secs" {
            args.push("30");
        }
        args.extend(["--", "sleep", "60"]);
        assert!(
            Cli::try_parse_from(args).is_err(),
            "{flag} must require --background"
        );
    }
}

#[test]
fn workspace_keep_requires_an_explicit_change() {
    let parsed = Cli::try_parse_from(["enclave", "workspace", "keep", "box", "api"]);
    assert!(
        parsed.is_err(),
        "keep must not silently change the pin or windows"
    );

    let parsed = Cli::try_parse_from([
        "enclave",
        "workspace",
        "keep",
        "box",
        "api",
        "--keep-running",
        "--no-keep-running",
    ]);
    assert!(parsed.is_err(), "a pin cannot be both set and cleared");
}

#[test]
fn workspace_keep_parses_independent_pin_and_window_changes() {
    let cli = Cli::parse_from([
        "enclave",
        "workspace",
        "keep",
        "box",
        "api",
        "--idle-stop-after-secs",
        "120",
        "--destroy-after-secs",
        "3600",
    ]);
    let Commands::Workspace {
        command: WorkspaceCommands::Keep(args),
    } = cli.command
    else {
        panic!("expected workspace keep");
    };
    assert!(!args.keep_running);
    assert!(!args.no_keep_running);
    assert_eq!(args.idle_stop_after_secs, Some(120));
    assert_eq!(args.destroy_after_secs, Some(3600));
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
