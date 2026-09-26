//! Tests for the command line.
//!
//! These parse arguments rather than run anything, so they are the one place where the
//! shape of the interface is pinned: a flag that stops parsing, or a default that moves,
//! is caught here rather than by a user.

use clap::Parser;
use enclave::cli::{
    Cli, Commands, RootfsCommands, SnapshotCommands, WorkspaceCommands, WorkspacePortCommands,
};

// A test target whose root is this file resolves its submodules beside it, so the
// paths are explicit. This is how the other multi-file test trees in this repository
// are mounted as well.
#[path = "cli/cp.rs"]
mod cp;
#[path = "cli/inspect.rs"]
mod inspect;
#[path = "cli/other.rs"]
mod other;
#[path = "cli/workspace.rs"]
mod workspace;
