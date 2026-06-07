mod auth;
mod daemon;
mod enclavefile;
mod internal;
mod policy;
mod ps;
mod registry;
mod rootfs;
mod sandbox;
mod stats;
mod workspace;

use std::io::Write;
use std::path::Path;

use anyhow::{bail, Result};
use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches};
use serde_json::json;

use crate::cli::{Cli, Commands, DaemonCommands, InternalCommands};
use crate::config::FileConfig;

pub fn run() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    let first_arg_is_help = args.get(1).is_some_and(|a| a == "help");
    let first_arg_is_init = args.get(1).is_some_and(|a| a == "init");
    let has_help_or_version_flag = args
        .iter()
        .skip(1)
        .take_while(|a| *a != "--")
        .any(|a| a == "--help" || a == "-h" || a == "--version" || a == "-V");
    let bypass_root_check = first_arg_is_help || first_arg_is_init;
    let is_help_or_version = bypass_root_check || has_help_or_version_flag;
    if !is_help_or_version {
        ensure_root_execution()?;
    }

    let matches = Cli::command().get_matches();
    let mut cli = Cli::from_arg_matches(&matches)?;
    let file_config = crate::config::load_config(cli.config.as_deref())?;
    apply_config_defaults(&mut cli, &matches, &file_config);
    daemon::configure_automatic_start_defaults(&file_config);
    match cli.command {
        Commands::Internal { command } => match *command {
            InternalCommands::WorkspaceSessionLaunch(args) => {
                internal::run_workspace_session_launch(*args)
            }
            InternalCommands::WorkspaceSessionBootstrap(args) => {
                internal::run_workspace_session_bootstrap(args)
            }
            InternalCommands::WorkspaceSessionLoop(args) => {
                internal::run_workspace_session_loop(args)
            }
            InternalCommands::WorkspaceCommand(args) => internal::run_workspace_command(args),
        },
        Commands::Daemon { command } => daemon::run_daemon_command(&cli.socket, command),
        Commands::Ping => {
            send(&cli.socket, "ping", json!({}))?;
            println!("pong");
            Ok(())
        }
        Commands::Health => {
            let result = send(&cli.socket, "daemon.health", json!({}))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        Commands::Doctor => {
            let result = send(&cli.socket, "daemon.doctor", json!({}))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        Commands::Init => enclavefile::run_init(),
        Commands::Up(args) => enclavefile::run_up(&cli.socket, args),
        Commands::Down => enclavefile::run_down(&cli.socket),
        Commands::Restart(args) => enclavefile::run_restart(&cli.socket, args),
        Commands::Create(args) => sandbox::run_create(&cli.socket, args),
        Commands::Start { sandbox } => sandbox::run_start(&cli.socket, &sandbox),
        Commands::Stop { sandbox } => sandbox::run_stop(&cli.socket, &sandbox),
        Commands::Destroy { sandbox } => sandbox::run_destroy(&cli.socket, &sandbox),
        Commands::List => sandbox::run_list(&cli.socket),
        Commands::Stats => stats::run_stats(&cli.socket),
        Commands::Ps(args) => ps::run_ps(&cli.socket, args),
        Commands::Status { sandbox } => sandbox::run_status(&cli.socket, &sandbox),
        Commands::Remove { sandbox_id } => sandbox::run_remove(&cli.socket, &sandbox_id),
        Commands::Wipe => sandbox::run_wipe(&cli.socket),
        Commands::Workspace { command } => workspace::run_workspace_command(&cli.socket, command),
        Commands::Snapshot { command } => workspace::run_snapshot_command(&cli.socket, command),
        Commands::Registry { command } => registry::run_registry_command(&cli.socket, command),
        Commands::Rootfs { command } => rootfs::run_rootfs_command(command),
        Commands::Auth { command } => auth::run_auth_command(command),
        Commands::Policy { command } => policy::run_policy_command(&cli.socket, command),
    }
}

fn ensure_root_execution() -> Result<()> {
    let euid = unsafe { libc::geteuid() };
    if euid == 0 {
        return Ok(());
    }
    bail!("enclave commands require root privileges. Re-run with sudo.");
}

fn apply_config_defaults(cli: &mut Cli, matches: &ArgMatches, file_config: &FileConfig) {
    if arg_uses_default(matches, "socket") {
        if let Some(socket) = file_config.socket.as_ref() {
            cli.socket = socket.clone();
        }
    }

    match &mut cli.command {
        Commands::Daemon { command } => match command {
            DaemonCommands::Run(args) => {
                let run_matches = nested_subcommand_matches(matches, &["daemon", "run"]);
                apply_daemon_common_defaults(args, run_matches, file_config);
            }
            DaemonCommands::Start(args) => {
                let start_matches = nested_subcommand_matches(matches, &["daemon", "start"]);
                apply_daemon_common_defaults(args, start_matches, file_config);
                if arg_uses_default_opt(start_matches, "wait_secs") {
                    if let Some(wait_secs) = file_config.wait_secs {
                        args.wait_secs = wait_secs;
                    }
                }
            }
            DaemonCommands::Stop | DaemonCommands::Status => {}
        },
        Commands::Create(args) => {
            let create_matches = nested_subcommand_matches(matches, &["create"]);
            if arg_uses_default_opt(create_matches, "suite") {
                if let Some(suite) = file_config.suite.as_ref() {
                    args.suite = suite.clone();
                }
            }
            if arg_uses_default_opt(create_matches, "mirror") {
                if let Some(mirror) = file_config.mirror.as_ref() {
                    args.mirror = mirror.clone();
                }
            }
            if arg_uses_default_opt(create_matches, "bootstrap_method") {
                if let Some(method_str) = file_config.bootstrap_method.as_ref() {
                    match method_str.parse() {
                        Ok(method) => args.bootstrap_method = method,
                        Err(err) => {
                            tracing::warn!(
                                "ignoring invalid bootstrap_method '{}' in config: {err}",
                                method_str
                            );
                        }
                    }
                }
            }
        }
        Commands::Rootfs { command } => match command {
            crate::cli::RootfsCommands::Export(args) => {
                let export_matches = nested_subcommand_matches(matches, &["rootfs", "export"]);
                apply_state_dir_default(args, export_matches, file_config);
            }
            crate::cli::RootfsCommands::Import(args) => {
                let import_matches = nested_subcommand_matches(matches, &["rootfs", "import"]);
                apply_state_dir_default(args, import_matches, file_config);
            }
            crate::cli::RootfsCommands::Fetch(args) => {
                let fetch_matches = nested_subcommand_matches(matches, &["rootfs", "fetch"]);
                apply_state_dir_default(args, fetch_matches, file_config);
            }
        },
        _ => {}
    }
}

fn apply_state_dir_default(
    args: &mut impl StateDirDefault,
    matches: Option<&ArgMatches>,
    file_config: &FileConfig,
) {
    if arg_uses_default_opt(matches, "state_dir") {
        if let Some(state_dir) = file_config.state_dir.as_ref() {
            args.state_dir_mut().clone_from(state_dir);
        }
    }
}

fn apply_daemon_common_defaults(
    args: &mut impl DaemonDefaults,
    matches: Option<&ArgMatches>,
    file_config: &FileConfig,
) {
    if arg_uses_default_opt(matches, "state_dir") {
        if let Some(state_dir) = file_config.state_dir.as_ref() {
            args.state_dir_mut().clone_from(state_dir);
        }
    }
    if arg_uses_default_opt(matches, "pid_file") {
        if let Some(pid_file) = file_config.pid_file.as_ref() {
            args.pid_file_mut().clone_from(pid_file);
        }
    }
    if arg_uses_default_opt(matches, "debootstrap_binary") {
        if let Some(binary) = file_config.debootstrap_binary.as_ref() {
            args.debootstrap_binary_mut().clone_from(binary);
        }
    }
    if arg_is_unset_opt(matches, "workspace_apparmor_profile") {
        if let Some(profile) = file_config.workspace_apparmor_profile.as_ref() {
            *args.workspace_apparmor_profile_mut() = Some(profile.clone());
        }
    }
    if arg_is_unset_opt(matches, "workspace_selinux_label") {
        if let Some(label) = file_config.workspace_selinux_label.as_ref() {
            *args.workspace_selinux_label_mut() = Some(label.clone());
        }
    }
}

trait DaemonDefaults {
    fn state_dir_mut(&mut self) -> &mut std::path::PathBuf;
    fn pid_file_mut(&mut self) -> &mut std::path::PathBuf;
    fn debootstrap_binary_mut(&mut self) -> &mut String;
    fn workspace_apparmor_profile_mut(&mut self) -> &mut Option<String>;
    fn workspace_selinux_label_mut(&mut self) -> &mut Option<String>;
}

trait StateDirDefault {
    fn state_dir_mut(&mut self) -> &mut std::path::PathBuf;
}

impl DaemonDefaults for crate::cli::RunArgs {
    fn state_dir_mut(&mut self) -> &mut std::path::PathBuf {
        &mut self.state_dir
    }
    fn pid_file_mut(&mut self) -> &mut std::path::PathBuf {
        &mut self.pid_file
    }
    fn debootstrap_binary_mut(&mut self) -> &mut String {
        &mut self.debootstrap_binary
    }
    fn workspace_apparmor_profile_mut(&mut self) -> &mut Option<String> {
        &mut self.workspace_apparmor_profile
    }
    fn workspace_selinux_label_mut(&mut self) -> &mut Option<String> {
        &mut self.workspace_selinux_label
    }
}

impl DaemonDefaults for crate::cli::StartArgs {
    fn state_dir_mut(&mut self) -> &mut std::path::PathBuf {
        &mut self.state_dir
    }
    fn pid_file_mut(&mut self) -> &mut std::path::PathBuf {
        &mut self.pid_file
    }
    fn debootstrap_binary_mut(&mut self) -> &mut String {
        &mut self.debootstrap_binary
    }
    fn workspace_apparmor_profile_mut(&mut self) -> &mut Option<String> {
        &mut self.workspace_apparmor_profile
    }
    fn workspace_selinux_label_mut(&mut self) -> &mut Option<String> {
        &mut self.workspace_selinux_label
    }
}

impl StateDirDefault for crate::cli::RootfsExportArgs {
    fn state_dir_mut(&mut self) -> &mut std::path::PathBuf {
        &mut self.state_dir
    }
}

impl StateDirDefault for crate::cli::RootfsImportArgs {
    fn state_dir_mut(&mut self) -> &mut std::path::PathBuf {
        &mut self.state_dir
    }
}

impl StateDirDefault for crate::cli::RootfsFetchArgs {
    fn state_dir_mut(&mut self) -> &mut std::path::PathBuf {
        &mut self.state_dir
    }
}

fn nested_subcommand_matches<'a>(
    matches: &'a ArgMatches,
    chain: &[&str],
) -> Option<&'a ArgMatches> {
    let mut current = matches;
    for name in chain {
        let (sub_name, sub_matches) = current.subcommand()?;
        if sub_name != *name {
            return None;
        }
        current = sub_matches;
    }
    Some(current)
}

fn arg_uses_default(matches: &ArgMatches, arg_name: &str) -> bool {
    matches.value_source(arg_name) == Some(ValueSource::DefaultValue)
}

fn arg_uses_default_opt(matches: Option<&ArgMatches>, arg_name: &str) -> bool {
    matches
        .and_then(|m| m.value_source(arg_name))
        .map(|source| source == ValueSource::DefaultValue)
        .unwrap_or(false)
}

fn arg_is_unset_opt(matches: Option<&ArgMatches>, arg_name: &str) -> bool {
    matches.and_then(|m| m.value_source(arg_name)).is_none()
}

pub(crate) fn send(
    socket: &Path,
    action: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    crate::client::send_request(socket, action, params)
}

pub(crate) fn send_managed(
    socket: &Path,
    action: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    daemon::ensure_daemon_running(socket)?;
    send(socket, action, params)
}

pub(crate) fn confirm_destructive_action(summary: &str, final_phrase: &str) -> Result<bool> {
    eprintln!("{summary}");
    tracing::warn!("{summary}");

    if !prompt_exact("Type 'y' then press Enter to continue: ", "y")? {
        return Ok(false);
    }

    let second_prompt = format!("Type '{}' then press Enter to confirm: ", final_phrase);
    if !prompt_exact(&second_prompt, final_phrase)? {
        return Ok(false);
    }

    Ok(true)
}

fn prompt_exact(prompt: &str, expected: &str) -> Result<bool> {
    eprint!("{prompt}");
    std::io::stderr().flush()?;

    let mut input = String::new();
    let read = std::io::stdin().read_line(&mut input)?;
    if read == 0 {
        return Ok(false);
    }

    Ok(input.trim() == expected)
}
