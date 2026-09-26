//! The registry transitions the workspace lifecycle operations share.
//!
//! The concerns are separate enough to live apart. The record submodule writes
//! the registry record and its on-disk metadata, cgroups owns the sandbox cgroup
//! that outlives its workspaces, resolve turns a user's selector into an id, and
//! reconcile repairs a record that disagrees with what is actually running.

mod cgroups;
mod reconcile;
mod record;
mod resolve;

pub(crate) use cgroups::{remove_sandbox_cgroup, remove_sandbox_cgroup_if_idle};
pub(crate) use reconcile::reconcile_workspace_runtime_state;
pub(crate) use record::{
    commit_workspace_stopped, mark_workspace_stopped, normalize_namespace_ref_paths,
    persist_workspace_metadata, set_workspace_stopped,
};
pub(crate) use resolve::resolve_workspace_id;
