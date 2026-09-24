mod admin;
mod common;
mod daemon;
mod session;
mod workspace;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::paths;

use common::parse_entity_name;

pub use admin::{
    PolicyClearArgs, PolicyDefaultArgs, PolicyRuleArgs, RegistryRepairArgs, RootfsExportArgs,
    RootfsFetchArgs, RootfsImportArgs,
};
pub use common::{AuthProviderArgs, CreateArgs, DoctorArgs, PsArgs, RestartArgs, UpArgs};
pub use daemon::{RunArgs, StartArgs};
pub use session::{
    WorkspaceCommandInternalArgs, WorkspaceFileReceiveArgs, WorkspaceSessionBootstrapArgs,
    WorkspaceSessionLaunchArgs, WorkspaceSessionLoopArgs, WorkspaceSessionPersistentHelperArgs,
};
pub use workspace::{
    WorkspaceCpArgs, WorkspaceCreateArgs, WorkspaceEnterArgs, WorkspaceExecArgs, WorkspaceListArgs,
    WorkspaceLogsArgs, WorkspacePortPublishArgs, WorkspacePortUnpublishArgs, WorkspaceRemoveArgs,
    WorkspaceResizeArgs, WorkspaceRestoreArgs, WorkspaceSnapshotArgs, WorkspaceSnapshotExportArgs,
    WorkspaceSnapshotGcArgs, WorkspaceSnapshotImportArgs, WorkspaceTargetArgs,
    WorkspaceTargetOrLocalArgs,
};

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
    #[arg(long, global = true)]
    pub start_daemon: bool,
    #[arg(long, global = true, help = "emit phase timing diagnostics to stderr")]
    pub verbose: bool,
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
    Doctor(DoctorArgs),
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
    Pause {
        #[arg(value_parser = parse_entity_name)]
        sandbox: String,
    },
    Resume {
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
    WorkspaceSessionPersistentHelper(WorkspaceSessionPersistentHelperArgs),
    WorkspaceCommand(WorkspaceCommandInternalArgs),
    WorkspaceFileReceive(WorkspaceFileReceiveArgs),
}

#[derive(Subcommand, Debug)]
pub enum WorkspaceCommands {
    Create(WorkspaceCreateArgs),
    Resize(WorkspaceResizeArgs),
    Cp(WorkspaceCpArgs),
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

pub(super) fn default_socket_arg() -> PathBuf {
    paths::default_socket_path()
}

pub(super) fn default_state_dir_arg() -> PathBuf {
    paths::default_state_dir()
}

pub(super) fn default_pid_file_arg() -> PathBuf {
    paths::default_pid_file()
}

pub(super) fn default_snapshot_keep() -> usize {
    crate::workspace::DEFAULT_SNAPSHOT_KEEP
}
