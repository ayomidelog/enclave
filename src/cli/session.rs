use clap::Args;

#[derive(Args, Debug)]
pub struct WorkspaceSessionInitArgs {
    #[arg(long, value_name = "PATH")]
    pub rootfs: String,
    #[arg(long, value_name = "PATH")]
    pub workspace_fs: String,
    #[arg(long)]
    pub workspace_id: String,
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
    #[arg(long, default_value_t = false)]
    pub disk_backed_tmp: bool,
    #[arg(long, default_value = "")]
    pub root_overlay_upper: String,
    #[arg(long, default_value = "")]
    pub root_overlay_work: String,
    #[arg(long, default_value = "")]
    pub root_overlay_merged: String,
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
    pub workspace_id: String,
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
    #[arg(long, default_value_t = false)]
    pub disk_backed_tmp: bool,
    #[arg(long, default_value = "")]
    pub root_overlay_upper: String,
    #[arg(long, default_value = "")]
    pub root_overlay_work: String,
    #[arg(long, default_value = "")]
    pub root_overlay_merged: String,
}

#[derive(Args, Debug)]
pub struct WorkspaceSessionBootstrapArgs {
    #[arg(long, value_name = "PATH")]
    pub rootfs: String,
    #[arg(long, value_name = "PATH")]
    pub workspace_fs: String,
    #[arg(long)]
    pub workspace_id: String,
    #[arg(long)]
    pub mount_target: String,
    #[arg(long, default_value = "")]
    pub workspace_idmap_option: String,
    #[arg(long, default_value_t = false)]
    pub disk_backed_tmp: bool,
    #[arg(long, default_value = "")]
    pub root_overlay_upper: String,
    #[arg(long, default_value = "")]
    pub root_overlay_work: String,
    #[arg(long, default_value = "")]
    pub root_overlay_merged: String,
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
pub struct WorkspaceSessionPersistentHelperArgs {
    #[arg(long = "helper-socket")]
    pub helper_socket: String,
    #[arg(long)]
    pub runtime_pid: u32,
    #[arg(long)]
    pub runtime_starttime_ticks: u64,
    #[arg(long)]
    pub runtime_pidfd: i32,
    #[arg(long)]
    pub sandbox_id: String,
    #[arg(long)]
    pub workspace_id: String,
    #[arg(long)]
    pub auth_token: String,
    #[arg(long, default_value = "")]
    pub cgroup_path: String,
    #[arg(long)]
    pub root_fd: i32,
    #[arg(long)]
    pub user_ns_fd: i32,
    #[arg(long)]
    pub mount_ns_fd: i32,
    #[arg(long)]
    pub pid_ns_fd: i32,
    #[arg(long)]
    pub net_ns_fd: i32,
    #[arg(long)]
    pub uts_ns_fd: i32,
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
    #[arg(long, default_value = "")]
    pub cgroup_path: String,
    #[arg(long)]
    pub root_fd: Option<i32>,
    #[arg(long)]
    pub user_ns_fd: Option<i32>,
    #[arg(long)]
    pub mount_ns_fd: Option<i32>,
    #[arg(long)]
    pub pid_ns_fd: Option<i32>,
    #[arg(long)]
    pub net_ns_fd: Option<i32>,
    #[arg(long)]
    pub uts_ns_fd: Option<i32>,
    #[arg(value_name = "COMMAND", required = true, num_args = 1.., trailing_var_arg = true)]
    pub command: Vec<String>,
}

#[derive(Args, Debug)]
pub struct WorkspaceFileReceiveArgs {
    #[arg(long)]
    pub runtime_pid: u32,
    #[arg(long)]
    pub runtime_starttime_ticks: u64,
    #[arg(long)]
    pub target: String,
    #[arg(long, default_value = "")]
    pub cgroup_path: String,
    #[arg(long)]
    pub root_fd: Option<i32>,
    #[arg(long)]
    pub user_ns_fd: Option<i32>,
    #[arg(long)]
    pub mount_ns_fd: Option<i32>,
    #[arg(long)]
    pub pid_ns_fd: Option<i32>,
    #[arg(long)]
    pub net_ns_fd: Option<i32>,
    #[arg(long)]
    pub uts_ns_fd: Option<i32>,
}
