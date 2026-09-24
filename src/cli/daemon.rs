use std::path::PathBuf;

use clap::Args;

use super::common::parse_non_empty_arg;
use super::{default_pid_file_arg, default_state_dir_arg};

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
