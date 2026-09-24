use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::unistd::{fork, ForkResult};

use crate::cli::{
    WorkspaceSessionBootstrapArgs, WorkspaceSessionLaunchArgs, WorkspaceSessionLoopArgs,
};

mod launch;
mod mounts;
mod rootfs;
mod tmp;

pub(crate) use launch::{
    run_workspace_session_launch, run_workspace_session_loop, run_workspace_session_loop_inner,
};
pub(crate) use rootfs::run_workspace_session_bootstrap;

// Used by `commands::internal` and by its tests.
#[cfg(test)]
pub(super) use rootfs::workspace_old_root_path;
pub(crate) use rootfs::{open_ready_file_via_old_root, path_inside_old_root};
pub(crate) use tmp::{runtime_tmpfs_mount_flags, RUNTIME_TMPFS_DATA};
#[cfg(test)]
pub(super) use tmp::{tmp_directory_is_usable, verify_workspace_tmp_mount};
#[cfg(test)]
pub(super) use tmp::{DirectoryIdentity, WORKSPACE_TMP_DATA};

// Shared between the session modules above.
pub(crate) use mounts::{is_mountpoint, mount_post_pivot_filesystems, mount_workspace_source};
pub(crate) use tmp::mount_workspace_tmp_if_needed;
