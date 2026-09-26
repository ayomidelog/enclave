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

/// Change a workspace's disk allocation, its memory limit, or both.
///
/// Each target is an absolute size in MiB, not a delta, and an omitted one is left
/// alone. Both can be raised or lowered.
///
/// The two limits are applied differently, because they are different things. Memory
/// is a cgroup value, so it is written to the running runtime and a workspace that is
/// already running is not interrupted for it. Disk is an image and the filesystem
/// inside it, which can only be resized with the workspace stopped, so a disk change
/// stops the workspace and starts it again through the normal lifecycle. Asking for
/// both does one stop rather than two.
///
/// A shrink is refused before anything is written when the filesystem holds more data
/// than the target, and the message names the smallest allocation that would work.
#[derive(Args, Debug)]
#[command(group(
    clap::ArgGroup::new("target")
        .args(["disk_mb", "memory_mb", "no_memory_limit"])
        .required(true)
        .multiple(true)
))]
pub struct WorkspaceResizeArgs {
    /// The sandbox the workspace is in.
    #[arg(value_parser = parse_entity_name)]
    pub sandbox: String,
    /// The workspace to resize, by id or name.
    #[arg(value_parser = parse_entity_name)]
    pub workspace: String,
    /// Target disk allocation in MiB. Omit to leave the disk alone.
    #[arg(long, value_name = "MIB")]
    pub disk_mb: Option<u64>,
    /// Target memory limit in MiB. Omit to leave memory alone.
    #[arg(long, value_name = "MIB", conflicts_with = "no_memory_limit")]
    pub memory_mb: Option<u64>,
    /// Remove the memory limit, leaving the workspace bounded only by the host.
    ///
    /// Spelled as its own flag rather than as a size, because a size of zero is a
    /// mistake rather than a request to remove the limit and is refused as one.
    #[arg(long)]
    pub no_memory_limit: bool,
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
pub struct WorkspaceDestroyArgs {
    #[arg(value_parser = parse_entity_name)]
    pub sandbox: String,
    #[arg(value_parser = parse_entity_name)]
    pub workspace: String,
    /// Remove the registry record even when host resources could not be released
    #[arg(long, default_value_t = false)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct WorkspaceWipeArgs {
    /// Remove registry records even when host resources could not be released
    #[arg(long, default_value_t = false)]
    pub force: bool,
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
