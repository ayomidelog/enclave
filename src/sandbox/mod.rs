mod bootstrap;
#[allow(dead_code)]
pub mod cgroup;
mod features;
mod lifecycle;
mod mounts;
mod types;
mod util;

pub use lifecycle::{
    create_sandbox, create_sandbox_with_options, destroy_sandbox, exec_setup_command, init_storage,
    list_sandbox_items, sandbox_status, start_sandbox, stop_sandbox, update_sandbox_limits,
    SandboxCreateOptions,
};
pub use types::{
    BootstrapMethod, SandboxLimits, SandboxLimitsUpdate, SandboxListItem, SandboxMetadata,
    SandboxStatus, SandboxStatusReport, DEFAULT_DEBIAN_MIRROR, DEFAULT_DEBIAN_SUITE,
};
pub(crate) use util::validate_debootstrap_binary;
pub(crate) use util::validate_debootstrap_inputs;
pub use util::{
    effective_rootfs_path, ensure_sandbox_layout, normalize_sandbox_metadata, resolve_sandbox_id,
};
