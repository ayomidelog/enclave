use std::path::PathBuf;

use clap::Args;

use super::default_state_dir_arg;

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
