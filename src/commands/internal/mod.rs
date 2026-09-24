pub(super) mod runtime_init;
mod session;

mod command;
mod file_receive;
mod namespaces;
mod persistent;

// Shared prelude for the submodules above: they each start with `use super::*`,
// so the argument types, helper primitives, and namespace plumbing they need are
// declared once here instead of being repeated in every file.
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path};
use std::process::Command;

use anyhow::{bail, Context, Result};
use nix::sched::{setns, CloneFlags};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{fork, ForkResult, Pid};
use serde::{Deserialize, Serialize};

use crate::cli::{
    WorkspaceCommandInternalArgs, WorkspaceFileReceiveArgs, WorkspaceSessionPersistentHelperArgs,
};

pub(crate) use session::{
    run_workspace_session_bootstrap, run_workspace_session_launch, run_workspace_session_loop,
};

pub(crate) use command::run_workspace_command;
pub(crate) use file_receive::run_workspace_file_receive;
pub(crate) use persistent::run_workspace_session_persistent_helper;

// Shared between the submodules above.
pub(crate) use command::run_workspace_command_child;
pub(crate) use namespaces::{
    attach_helper_to_workspace_cgroup, enter_workspace_namespaces, enter_workspace_root,
    wait_pid_exit_code, NamespaceHandles,
};

#[cfg(test)]
pub(crate) use file_receive::validate_workspace_file_target;
#[cfg(test)]
use session::{
    open_ready_file_via_old_root, runtime_tmpfs_mount_flags, tmp_directory_is_usable,
    verify_workspace_tmp_mount, workspace_old_root_path, DirectoryIdentity, RUNTIME_TMPFS_DATA,
    WORKSPACE_TMP_DATA,
};

#[cfg(test)]
#[path = "../../../tests/src/commands/internal.rs"]
mod tests;
