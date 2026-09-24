use std::path::PathBuf;

use clap::Args;

use super::common::{
    parse_cpu_percent_arg, parse_entity_name, parse_non_empty_arg, parse_shell_path,
};
use super::default_snapshot_keep;

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
pub struct WorkspaceResizeArgs {
    #[arg(value_parser = parse_entity_name)]
    pub sandbox: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace: String,
    #[arg(long, value_name = "MIB")]
    pub disk_mb: u64,
}

#[derive(Args, Debug)]
pub struct WorkspaceCpArgs {
    #[arg(value_parser = parse_entity_name)]
    pub sandbox: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace: String,
    #[arg(value_parser = parse_non_empty_arg)]
    pub src: String,
    #[arg(value_parser = parse_non_empty_arg)]
    pub dst: String,
    #[arg(long, default_value_t = false)]
    pub progress: bool,
    #[arg(long, default_value_t = false)]
    pub gzip: bool,
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
