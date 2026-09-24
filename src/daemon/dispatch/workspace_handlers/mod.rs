//! The daemon-side workspace request handlers.
//!
//! The requests are grouped by what they act on: creating a workspace, driving one
//! through its lifecycle, changing its definition, and moving data in and out.

use super::*;
use crate::workspace::WorkspaceStatus;

mod batch;
mod create;
mod definition;
mod lifecycle;
mod transfer;

pub(super) use batch::dispatch_workspace_start_many;
#[cfg(test)]
pub(super) use batch::existing_workspace_update;
pub(super) use create::dispatch_workspace_create;
pub(super) use definition::{
    dispatch_workspace_list, dispatch_workspace_resize, dispatch_workspace_update,
};
pub(super) use lifecycle::dispatch_workspace_target;
pub(super) use transfer::{
    dispatch_workspace_cp, dispatch_workspace_exec, dispatch_workspace_logs,
};
