//! The command line: parsing, config defaults, and dispatch.
//!
//! `run` is the whole entry point. It parses the arguments, applies the config
//! file to the values the operator did not type, and hands the command to the
//! module that implements it. Each command module owns one command group and
//! prints its own result, so this file stays a table of contents.
//!
//! The modules are grouped by what a command acts on:
//!
//! - `sandbox`, `workspace`, `stats`, `ps`, and `registry` are the lifecycle and
//!   inspection commands.
//! - `enclavefile` is the `up`/`down`/`restart` path driven by an Enclavefile.
//! - `rootfs`, `auth`, `policy`, and `daemon` cover the rest of the CLI surface.
//! - `internal` is the helper entry points the daemon re-executes itself into.
//! - `config_defaults`, `send`, and `confirm` are what every command group shares.

mod auth;
mod config_defaults;
mod confirm;
mod daemon;
mod enclavefile;
mod internal;
mod policy;
mod ps;
mod registry;
mod rootfs;
mod sandbox;
mod send;
mod stats;
mod transition;
mod workspace;

use anyhow::{bail, Result};
use clap::{CommandFactory, FromArgMatches};
use serde_json::json;

use crate::cli::{Cli, Commands, InternalCommands};

pub(crate) use confirm::{confirm_destructive_action, report_retained_resources};
pub(crate) use send::{print_operation_id, send, send_managed};
pub(crate) use transition::print_state_transition;

pub fn run() -> Result<()> {
    let _cli_total = crate::perf::Timer::new("cli.total");
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
    crate::perf::set_verbose(cli.verbose);
    let _config_load = crate::perf::Timer::new("cli.config_load");
    let file_config = crate::config::load_config(cli.config.as_deref())?;
    config_defaults::apply_config_defaults(&mut cli, &matches, &file_config);
    daemon::configure_automatic_start_defaults(&file_config, cli.start_daemon);
    match cli.command {
        Commands::Internal { command } => match *command {
            InternalCommands::WorkspaceSessionLaunch(args) => {
                internal::run_workspace_session_launch(*args)
            }
            InternalCommands::WorkspaceSessionInit(args) => {
                internal::run_workspace_session_init(*args)
            }
            InternalCommands::WorkspaceSessionBootstrap(args) => {
                internal::run_workspace_session_bootstrap(args)
            }
            InternalCommands::WorkspaceSessionLoop(args) => {
                internal::run_workspace_session_loop(args)
            }
            InternalCommands::WorkspaceSessionPersistentHelper(args) => {
                internal::run_workspace_session_persistent_helper(args)
            }
            InternalCommands::WorkspaceCommand(args) => internal::run_workspace_command(args),
            InternalCommands::WorkspaceFileReceive(args) => {
                internal::run_workspace_file_receive(args)
            }
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
        Commands::Doctor(args) => {
            let action = if args.repair {
                "daemon.doctor.repair"
            } else {
                "daemon.doctor"
            };
            let result = if args.repair {
                send_managed(&cli.socket, action, json!({}))?
            } else {
                send(&cli.socket, action, json!({}))?
            };
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
        Commands::Pause { sandbox } => sandbox::run_pause(&cli.socket, &sandbox),
        Commands::Resume { sandbox } => sandbox::run_resume(&cli.socket, &sandbox),
        Commands::Destroy(args) => sandbox::run_destroy(&cli.socket, args),
        Commands::List => sandbox::run_list(&cli.socket),
        Commands::Stats => stats::run_stats(&cli.socket),
        Commands::Ps(args) => ps::run_ps(&cli.socket, args),
        Commands::Status { sandbox } => sandbox::run_status(&cli.socket, &sandbox),
        Commands::Remove { sandbox_id } => sandbox::run_remove(&cli.socket, &sandbox_id),
        Commands::Wipe(args) => sandbox::run_wipe(&cli.socket, args),
        Commands::Workspace { command } => workspace::run_workspace_command(&cli.socket, command),
        Commands::Snapshot { command } => workspace::run_snapshot_command(&cli.socket, command),
        Commands::Registry { command } => registry::run_registry_command(&cli.socket, command),
        Commands::Rootfs { command } => rootfs::run_rootfs_command(command),
        Commands::Auth { command } => auth::run_auth_command(command),
        Commands::Policy { command } => policy::run_policy_command(&cli.socket, command),
    }
}

/// Refuse to run anything that touches host state as a non-root user.
///
/// `--help`, `--version`, and `init` are exempt: they read nothing and change
/// nothing, and requiring root to see the help text is a poor first impression.
fn ensure_root_execution() -> Result<()> {
    let euid = unsafe { libc::geteuid() };
    if euid == 0 {
        return Ok(());
    }
    bail!("enclave commands require root privileges. Re-run with sudo.");
}
