//! Removing a workspace and everything it owns.
//!
//! A destroy is a stop that also removes the files and the record, and it owes the
//! same proof a stop does: the certificate it returns is checked against the host
//! rather than inferred from the calls it made. The concerns are separate modules
//! because a caller reaches for them separately. `one` releases a single workspace
//! and is where the re-resolve loop lives, `batch` runs the same work over every
//! workspace for a wipe, and `report` holds what both return.

mod batch;
mod one;
mod report;

pub use batch::{destroy_all_workspaces, BatchDestroyReport};
pub use one::{destroy_workspace, destroy_workspace_with_mode, remove_workspace};
pub use report::WorkspaceDestroyReport;
