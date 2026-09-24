//! The sandbox command group.
//!
//! A sandbox is a shared root filesystem with a set of workspaces; these commands
//! create it, change its lifecycle, inspect it, and delete it. The work is split
//! by what the command is for:
//!
//! - `create` runs the bootstrap and streams its output while it runs.
//! - `lifecycle` is the state-changing commands: start, stop, pause, resume,
//!   destroy.
//! - `inspect` is the read-only and bulk commands: list, status, remove, wipe.

mod create;
mod inspect;
mod lifecycle;

pub(crate) use create::run_create;
pub(crate) use inspect::{run_list, run_remove, run_status, run_wipe};
pub(crate) use lifecycle::{run_destroy, run_pause, run_resume, run_start, run_stop};
