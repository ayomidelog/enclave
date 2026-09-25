//! Sandbox identity, layout, and the host programs a sandbox runs.
//!
//! A sandbox is a directory with a fixed shape and a name that has to be
//! usable as a selector, plus a bootstrap program that runs for minutes. The
//! three concerns are separate: the name and id in the identity module, the
//! directory shape in the layout module, and the bootstrap program and its
//! live log in the command module.

mod command;
mod identity;
mod layout;

pub(crate) use command::{command_failure_detail, run_command_with_live_log};
pub use identity::resolve_sandbox_id;
pub(crate) use identity::{
    generate_sandbox_id, sandboxes_dir, validate_debootstrap_binary, validate_debootstrap_inputs,
    validate_name,
};
pub(crate) use layout::dir_size;
pub use layout::{effective_rootfs_path, ensure_sandbox_layout, normalize_sandbox_metadata};
