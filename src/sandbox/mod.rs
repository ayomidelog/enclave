mod bootstrap;
mod cache;
#[allow(dead_code)]
pub mod cgroup;
mod features;
mod lifecycle;
mod mounts;
mod setup_cache;
mod types;
mod util;

pub(crate) use bootstrap::{ensure_rootfs_cache, has_rootfs_content, rootfs_cache_dir};
pub(crate) use cache::content_identity as rootfs_cache_content_identity;
pub(crate) use cache::register as register_rootfs_cache;
pub use lifecycle::{
    create_sandbox, create_sandbox_with_options, destroy_sandbox, destroy_sandbox_with_mode,
    exec_setup_command, init_storage, list_sandbox_items, pause_sandbox, reconcile_runtime_state,
    resume_sandbox, sandbox_status, start_sandbox, stop_sandbox, update_sandbox_limits,
    SandboxCreateOptions, SandboxDestroyReport,
};
pub(crate) use mounts::ensure_rootfs_ready_for_workspace;
pub use types::{
    BootstrapMethod, RootfsTier, SandboxLimits, SandboxLimitsUpdate, SandboxListItem,
    SandboxMetadata, SandboxStatus, SandboxStatusReport, DEFAULT_DEBIAN_MIRROR,
    DEFAULT_DEBIAN_SUITE,
};
pub(crate) use util::validate_debootstrap_inputs;
pub use util::{
    effective_rootfs_path, ensure_sandbox_layout, normalize_sandbox_metadata, resolve_sandbox_id,
};
