use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::paths;
use crate::resource_limits::validate_cpu_percent;
use crate::sandbox::{BootstrapMethod, DEFAULT_DEBIAN_MIRROR, DEFAULT_DEBIAN_SUITE};

#[derive(Parser, Debug)]
#[command(
    name = "enclave",
    version,
    about = "Enclave Linux workspace isolation platform"
)]
pub struct Cli {
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    #[arg(long, global = true, value_name = "PATH", default_value_os_t = default_socket_arg())]
    pub socket: PathBuf,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: Box<InternalCommands>,
    },
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },
    Ping,
    Health,
    Doctor,
    Init,
    Up(UpArgs),
    Down,
    Restart(RestartArgs),
    Create(CreateArgs),
    Start {
        #[arg(value_parser = parse_entity_name)]
        sandbox: String,
    },
    Stop {
        #[arg(value_parser = parse_entity_name)]
        sandbox: String,
    },
    Destroy {
        #[arg(value_parser = parse_entity_name)]
        sandbox: String,
    },
    List,
    Stats,
    Ps(PsArgs),
    Status {
        #[arg(value_parser = parse_entity_name)]
        sandbox: String,
    },
    Remove {
        #[arg(value_parser = parse_entity_name)]
        sandbox_id: String,
    },
    Wipe,
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommands,
    },
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommands,
    },
    Registry {
        #[command(subcommand)]
        command: RegistryCommands,
    },
    Rootfs {
        #[command(subcommand)]
        command: RootfsCommands,
    },
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },
    Policy {
        #[command(subcommand)]
        command: PolicyCommands,
    },
}

#[derive(Subcommand, Debug)]
pub enum DaemonCommands {
    Run(RunArgs),
    Start(StartArgs),
    Stop,
    Status,
}

#[derive(Subcommand, Debug)]
pub enum InternalCommands {
    WorkspaceSessionLaunch(Box<WorkspaceSessionLaunchArgs>),
    WorkspaceSessionBootstrap(WorkspaceSessionBootstrapArgs),
    WorkspaceSessionLoop(WorkspaceSessionLoopArgs),
    WorkspaceCommand(WorkspaceCommandInternalArgs),
}

#[derive(Subcommand, Debug)]
pub enum WorkspaceCommands {
    Create(WorkspaceCreateArgs),
    List(WorkspaceListArgs),
    Remove(WorkspaceRemoveArgs),
    Wipe,
    Start(WorkspaceTargetArgs),
    Stop(WorkspaceTargetArgs),
    Destroy(WorkspaceTargetArgs),
    Status(WorkspaceTargetArgs),
    Stats(WorkspaceTargetOrLocalArgs),
    Enter(WorkspaceEnterArgs),
    Logs(WorkspaceLogsArgs),
    Snapshot(WorkspaceSnapshotArgs),
    SnapshotList(WorkspaceTargetArgs),
    Restore(WorkspaceRestoreArgs),
    SnapshotGc(WorkspaceSnapshotGcArgs),
    Exec(WorkspaceExecArgs),
    Run(WorkspaceExecArgs),
    Port {
        #[command(subcommand)]
        command: WorkspacePortCommands,
    },
}

#[derive(Subcommand, Debug)]
pub enum WorkspacePortCommands {
    Publish(WorkspacePortPublishArgs),
    Unpublish(WorkspacePortUnpublishArgs),
    List(WorkspaceTargetArgs),
}

#[derive(Subcommand, Debug)]
pub enum SnapshotCommands {
    Create(WorkspaceSnapshotArgs),
    List(WorkspaceTargetArgs),
    Restore(WorkspaceRestoreArgs),
    Export(WorkspaceSnapshotExportArgs),
    Import(WorkspaceSnapshotImportArgs),
}

#[derive(Subcommand, Debug)]
pub enum RegistryCommands {
    Repair(RegistryRepairArgs),
}

#[derive(Subcommand, Debug)]
pub enum RootfsCommands {
    Export(RootfsExportArgs),
    Import(RootfsImportArgs),
    Fetch(RootfsFetchArgs),
}

#[derive(Subcommand, Debug)]
pub enum PolicyCommands {
    Show,
    Default(PolicyDefaultArgs),
    Allow(PolicyRuleArgs),
    Deny(PolicyRuleArgs),
    Clear(PolicyClearArgs),
}

#[derive(Subcommand, Debug)]
pub enum AuthCommands {
    Login(AuthProviderArgs),
    List,
    Logout(AuthProviderArgs),
}

#[derive(Args, Debug)]
pub struct AuthProviderArgs {
    #[arg(value_parser = parse_entity_name)]
    pub provider: String,
}

#[derive(Args, Debug)]
pub struct RunArgs {
    #[arg(long, value_name = "PATH", default_value_os_t = default_state_dir_arg())]
    pub state_dir: PathBuf,
    #[arg(long, value_name = "PATH", default_value_os_t = default_pid_file_arg())]
    pub pid_file: PathBuf,
    #[arg(long, default_value = "debootstrap")]
    pub debootstrap_binary: String,
    #[arg(long, value_parser = parse_non_empty_arg, value_name = "PROFILE")]
    pub workspace_apparmor_profile: Option<String>,
    #[arg(long, value_parser = parse_non_empty_arg, value_name = "LABEL")]
    pub workspace_selinux_label: Option<String>,
}

#[derive(Args, Debug)]
pub struct StartArgs {
    #[arg(long, value_name = "PATH", default_value_os_t = default_state_dir_arg())]
    pub state_dir: PathBuf,
    #[arg(long, value_name = "PATH", default_value_os_t = default_pid_file_arg())]
    pub pid_file: PathBuf,
    #[arg(long, default_value = "debootstrap")]
    pub debootstrap_binary: String,
    #[arg(long, default_value_t = 5)]
    pub wait_secs: u64,
    #[arg(long, value_parser = parse_non_empty_arg, value_name = "PROFILE")]
    pub workspace_apparmor_profile: Option<String>,
    #[arg(long, value_parser = parse_non_empty_arg, value_name = "LABEL")]
    pub workspace_selinux_label: Option<String>,
}

#[derive(Args, Debug)]
pub struct UpArgs {
    #[arg(long)]
    pub rebuild: bool,
}

#[derive(Args, Debug)]
pub struct RestartArgs {
    #[arg(long)]
    pub rebuild: bool,
}

#[derive(Args, Debug)]
pub struct CreateArgs {
    #[arg(value_parser = parse_entity_name)]
    pub name: String,
    #[arg(long, default_value = DEFAULT_DEBIAN_SUITE)]
    pub suite: String,
    #[arg(long, default_value = DEFAULT_DEBIAN_MIRROR)]
    pub mirror: String,
    #[arg(long, default_value = "debootstrap", value_parser = parse_bootstrap_method)]
    pub bootstrap_method: BootstrapMethod,
    #[arg(long)]
    pub memory_mb: Option<u64>,
    #[arg(long, value_parser = parse_cpu_percent_arg)]
    pub cpu_percent: Option<f64>,
    #[arg(long)]
    pub max_procs: Option<u64>,
}

fn parse_bootstrap_method(s: &str) -> Result<BootstrapMethod, String> {
    s.parse::<BootstrapMethod>().map_err(|e| e.to_string())
}

fn parse_cpu_percent_arg(s: &str) -> Result<f64, String> {
    let value = s
        .parse::<f64>()
        .map_err(|_| "cpu_percent must be a number".to_string())?;
    validate_cpu_percent(value).map_err(|err| err.to_string())?;
    Ok(value)
}

const MAX_ENTITY_NAME_LEN: usize = 63;

fn parse_non_empty_arg(s: &str) -> Result<String, String> {
    if s.is_empty() {
        return Err("value must not be empty".to_string());
    }
    if s.chars().any(char::is_control) {
        return Err("value must not contain control characters".to_string());
    }
    Ok(s.to_string())
}

fn parse_entity_name(s: &str) -> Result<String, String> {
    if s.is_empty() || s.len() > MAX_ENTITY_NAME_LEN {
        return Err(format!("value must be 1-{MAX_ENTITY_NAME_LEN} characters"));
    }
    if s.chars().any(char::is_control) {
        return Err("value must not contain control characters".to_string());
    }
    let mut chars = s.chars();
    let first = chars
        .next()
        .ok_or_else(|| "value is required".to_string())?;
    if !first.is_ascii_alphanumeric() {
        return Err("value must start with an ASCII letter or digit".to_string());
    }
    if chars.any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
        return Err("value may only contain ASCII letters, digits, '-' and '_'".to_string());
    }
    Ok(s.to_string())
}

fn parse_shell_path(s: &str) -> Result<String, String> {
    if s.trim().is_empty() {
        return Err("shell path must not be empty".to_string());
    }
    if s.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("shell must be an absolute path without spaces or arguments".to_string());
    }
    if !s.starts_with('/') {
        return Err("shell must be an absolute path".to_string());
    }
    Ok(s.to_string())
}

#[derive(Args, Debug)]
pub struct WorkspaceCreateArgs {
    #[arg(value_parser = parse_entity_name)]
    pub sandbox_id: String,
    #[arg(value_parser = parse_entity_name)]
    pub name: String,
    #[arg(long)]
    pub cpu_seconds: Option<u64>,
    #[arg(long, value_parser = parse_cpu_percent_arg)]
    pub cpu_percent: Option<f64>,
    #[arg(long)]
    pub memory_mb: Option<u64>,
    #[arg(long)]
    pub max_procs: Option<u64>,
    #[arg(long)]
    pub max_open_files: Option<u64>,
    #[arg(long)]
    pub disk_mb: Option<u64>,
}

#[derive(Args, Debug)]
pub struct WorkspaceSessionLaunchArgs {
    #[arg(long, default_value_t = false)]
    pub enable_userns: bool,
    #[arg(long)]
    pub uid_inner: u32,
    #[arg(long)]
    pub uid_outer: u32,
    #[arg(long)]
    pub uid_count: u32,
    #[arg(long)]
    pub gid_inner: u32,
    #[arg(long)]
    pub gid_outer: u32,
    #[arg(long)]
    pub gid_count: u32,
    #[arg(long, default_value_t = false)]
    pub deny_setgroups: bool,
    #[arg(long, value_name = "PATH")]
    pub rootfs: String,
    #[arg(long, value_name = "PATH")]
    pub workspace_fs: String,
    #[arg(long)]
    pub mount_target: String,
    #[arg(long, value_name = "PATH")]
    pub mount_ref: String,
    #[arg(long, value_name = "PATH")]
    pub pid_ref: String,
    #[arg(long, value_name = "PATH")]
    pub pid_file: String,
    #[arg(long, value_name = "PATH")]
    pub ready_file: String,
    #[arg(long, default_value = "")]
    pub cpu_limit: String,
    #[arg(long, default_value = "")]
    pub memory_limit_kb: String,
    #[arg(long, default_value = "")]
    pub proc_limit: String,
    #[arg(long, default_value = "")]
    pub nofile_limit: String,
    #[arg(long, default_value = "")]
    pub workspace_hostname: String,
    #[arg(long, value_name = "PATH")]
    pub session_helper: String,
    #[arg(long, default_value = "")]
    pub apparmor_profile: String,
    #[arg(long, default_value = "")]
    pub selinux_label: String,
    #[arg(long, default_value = "")]
    pub workspace_idmap_option: String,
}

#[derive(Args, Debug)]
pub struct WorkspaceSessionBootstrapArgs {
    #[arg(long, value_name = "PATH")]
    pub rootfs: String,
    #[arg(long, value_name = "PATH")]
    pub workspace_fs: String,
    #[arg(long)]
    pub mount_target: String,
    #[arg(long, default_value = "")]
    pub workspace_idmap_option: String,
    #[arg(long, value_name = "PATH")]
    pub ready_file: String,
}

#[derive(Args, Debug)]
pub struct WorkspaceSessionLoopArgs {
    #[arg(long, value_name = "PATH")]
    pub old_root: String,
    #[arg(long, value_name = "PATH")]
    pub ready_file: String,
}

#[derive(Args, Debug)]
pub struct WorkspaceCommandInternalArgs {
    #[arg(long)]
    pub runtime_pid: u32,
    #[arg(long)]
    pub runtime_starttime_ticks: u64,
    #[arg(long)]
    pub cwd: String,
    #[arg(long)]
    pub sandbox_id: String,
    #[arg(long)]
    pub workspace_id: String,
    #[arg(value_name = "COMMAND", required = true, num_args = 1.., trailing_var_arg = true)]
    pub command: Vec<String>,
}

#[derive(Args, Debug)]
pub struct WorkspaceListArgs {
    #[arg(long, value_parser = parse_entity_name)]
    pub sandbox_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct WorkspaceRemoveArgs {
    #[arg(value_parser = parse_entity_name)]
    pub sandbox_id: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace_id: String,
}

#[derive(Args, Debug)]
pub struct WorkspaceTargetArgs {
    #[arg(value_parser = parse_entity_name)]
    pub sandbox: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace: String,
}

#[derive(Args, Debug)]
pub struct WorkspaceTargetOrLocalArgs {
    #[arg(value_parser = parse_entity_name)]
    pub target: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace: Option<String>,
}

#[derive(Args, Debug)]
pub struct WorkspaceExecArgs {
    #[arg(value_parser = parse_entity_name)]
    pub sandbox_id: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace_id: String,
    #[arg(long, default_value = "/home")]
    pub cwd: String,
    #[arg(required = true, trailing_var_arg = true, value_parser = parse_non_empty_arg)]
    pub command: Vec<String>,
}

#[derive(Args, Debug)]
pub struct WorkspacePortPublishArgs {
    #[arg(value_parser = parse_entity_name)]
    pub sandbox: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace: String,
    #[arg(value_parser = parse_non_empty_arg)]
    pub spec: String,
}

#[derive(Args, Debug)]
pub struct WorkspacePortUnpublishArgs {
    #[arg(value_parser = parse_entity_name)]
    pub sandbox: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace: String,
    #[arg(value_parser = parse_non_empty_arg)]
    pub binding: String,
}

#[derive(Args, Debug)]
pub struct PsArgs {
    #[arg(long, visible_alias = "project")]
    pub local: bool,
}

#[derive(Args, Debug)]
pub struct WorkspaceEnterArgs {
    #[arg(value_parser = parse_entity_name)]
    pub sandbox: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace: String,
    #[arg(long, default_value = "/home")]
    pub cwd: String,
    #[arg(long, value_parser = parse_shell_path)]
    pub shell: Option<String>,
}

#[derive(Args, Debug)]
pub struct WorkspaceLogsArgs {
    #[arg(value_parser = parse_entity_name)]
    pub target: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace: Option<String>,
    #[arg(long)]
    pub tail: Option<usize>,
    #[arg(long)]
    pub follow: bool,
}

#[derive(Args, Debug)]
pub struct WorkspaceSnapshotArgs {
    #[arg(value_parser = parse_entity_name)]
    pub sandbox: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace: String,
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Args, Debug)]
pub struct WorkspaceRestoreArgs {
    #[arg(value_parser = parse_entity_name)]
    pub sandbox: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace: String,
    pub snapshot: String,
}

#[derive(Args, Debug)]
pub struct WorkspaceSnapshotExportArgs {
    #[arg(value_parser = parse_entity_name)]
    pub sandbox: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace: String,
    #[arg(value_parser = parse_entity_name)]
    pub snapshot: String,
    #[arg(long, value_name = "PATH")]
    pub output: PathBuf,
}

#[derive(Args, Debug)]
pub struct WorkspaceSnapshotImportArgs {
    #[arg(value_parser = parse_entity_name)]
    pub sandbox: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace: String,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long, default_value_t = false)]
    pub replace: bool,
    #[arg(value_name = "ARCHIVE")]
    pub archive: PathBuf,
}

#[derive(Args, Debug)]
pub struct WorkspaceSnapshotGcArgs {
    #[arg(value_parser = parse_entity_name)]
    pub sandbox: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace: String,
    #[arg(long, default_value_t = default_snapshot_keep())]
    pub keep: usize,
}

#[derive(Args, Debug)]
pub struct RegistryRepairArgs {
    #[arg(long)]
    pub strict: bool,
}

#[derive(Args, Debug)]
pub struct RootfsExportArgs {
    #[arg(long, value_name = "PATH", default_value_os_t = default_state_dir_arg())]
    pub state_dir: PathBuf,
    #[arg(long)]
    pub suite: Option<String>,
    #[arg(long, default_value_t = false)]
    pub base: bool,
    #[arg(long, value_name = "PATH")]
    pub output: PathBuf,
}

#[derive(Args, Debug)]
pub struct RootfsImportArgs {
    #[arg(long, value_name = "PATH", default_value_os_t = default_state_dir_arg())]
    pub state_dir: PathBuf,
    #[arg(long)]
    pub suite: Option<String>,
    #[arg(long, default_value_t = false)]
    pub base: bool,
    #[arg(long, default_value_t = false)]
    pub replace: bool,
    #[arg(value_name = "ARCHIVE")]
    pub archive: PathBuf,
}

#[derive(Args, Debug)]
pub struct RootfsFetchArgs {
    #[arg(long, value_name = "PATH", default_value_os_t = default_state_dir_arg())]
    pub state_dir: PathBuf,
    #[arg(long)]
    pub suite: Option<String>,
    #[arg(long, default_value_t = false)]
    pub base: bool,
    #[arg(long, default_value_t = false)]
    pub replace: bool,
    #[arg(value_name = "URL")]
    pub url: String,
}

#[derive(Args, Debug)]
pub struct PolicyDefaultArgs {
    #[arg(value_parser = ["allow", "deny"])]
    pub mode: String,
}

#[derive(Args, Debug)]
pub struct PolicyRuleArgs {
    pub action: String,
    #[arg(long)]
    pub uid: Option<u32>,
}

#[derive(Args, Debug)]
pub struct PolicyClearArgs {
    #[arg(long)]
    pub uid: Option<u32>,
}

fn default_socket_arg() -> PathBuf {
    paths::default_socket_path()
}

fn default_state_dir_arg() -> PathBuf {
    paths::default_state_dir()
}

fn default_pid_file_arg() -> PathBuf {
    paths::default_pid_file()
}

fn default_snapshot_keep() -> usize {
    crate::workspace::DEFAULT_SNAPSHOT_KEEP
}
