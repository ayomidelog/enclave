//! Applying the config file to the parsed command line.
//!
//! A value in the config file is a default, not an override: it applies only
//! where the operator did not type the flag. That is what clap's value source
//! answers, so every assignment here is guarded by "the argument still came from
//! its default". A flag given on the command line always wins, which is the only
//! rule that lets an operator correct a config file for one invocation without
//! editing it.
//!
//! The guards are per argument rather than per command because a config file can
//! supply some values and not others, and the accessors are per argument because
//! the daemon flags live on two different argument structs that share a shape.

use clap::parser::ValueSource;
use clap::ArgMatches;

use crate::cli::{Cli, Commands, DaemonCommands};
use crate::config::FileConfig;

/// Fill in every value the config file supplies and the command line did not.
pub(crate) fn apply_config_defaults(cli: &mut Cli, matches: &ArgMatches, file_config: &FileConfig) {
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

/// The daemon flags shared by `daemon run` and `daemon start`.
///
/// The two commands take the same set of paths and labels, so the config
/// application is written once against these accessors instead of twice against
/// the structs.
trait DaemonDefaults {
    fn state_dir_mut(&mut self) -> &mut std::path::PathBuf;
    fn pid_file_mut(&mut self) -> &mut std::path::PathBuf;
    fn debootstrap_binary_mut(&mut self) -> &mut String;
    fn workspace_apparmor_profile_mut(&mut self) -> &mut Option<String>;
    fn workspace_selinux_label_mut(&mut self) -> &mut Option<String>;
}

/// The single path the rootfs commands share.
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

/// The argument matches of a nested subcommand, e.g. `daemon run`.
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
