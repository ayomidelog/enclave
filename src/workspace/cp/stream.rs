use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use tar::Archive;
use uuid::Uuid;

use super::path::DestinationPlan;
use crate::workspace::exec::{spawn_workspace_command, spawn_workspace_file_receiver};
use crate::workspace::types::WorkspaceMetadata;

mod archive;
mod hostfs;
mod process;
mod transfer;
mod utility;
mod workspace_fs;

// Used by `cp/mod.rs` and by the copy tests through `super::stream`.
pub(super) use archive::{
    extract_workspace_archive_with_stats, restore_workspace_metadata, write_host_directory_archive,
};
pub(super) use transfer::{run_host_to_workspace, run_workspace_to_host};
pub(super) use workspace_fs::{
    validate_workspace_source, workspace_path_exists, workspace_path_has_symlink,
    workspace_path_is_directory,
};

// Entry points the copy tests address through `super::stream`.
#[cfg(test)]
pub(super) use archive::{
    extract_workspace_archive, validate_archive_entry_type, validate_archive_path,
};
#[cfg(test)]
pub(super) use transfer::splice_to_pipe;

// Shared between the sibling modules above.
pub(crate) use archive::{tar_create_args, tar_extract_args_at, workspace_tar_command};
pub(crate) use hostfs::{cstring, ensure_host_entry_absent, open_host_directory};
pub(crate) use process::{set_pipe_capacity, wait_child_output};
pub(crate) use utility::{ensure_transfer_success, run_workspace_utility};
pub(crate) use workspace_fs::{
    create_workspace_staging_directory, move_workspace_path, remove_workspace_staging_directory,
};

struct TransferContext<'a> {
    gzip: bool,
    client_stream: Option<&'a UnixStream>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct UtilityCacheKey {
    sandbox_id: String,
    workspace_id: String,
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    command: String,
}

static UTILITY_CACHE: OnceLock<Mutex<HashMap<UtilityCacheKey, bool>>> = OnceLock::new();
const UTILITY_CACHE_LIMIT: usize = 512;

#[derive(Debug)]
pub(super) struct TransferOutput {
    pub(super) logical_bytes: u64,
    pub(super) files: u64,
}
pub(super) struct ChildGuard {
    child: Child,
    armed: bool,
}

impl ChildGuard {
    pub(super) fn new(child: Child) -> Self {
        Self { child, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub(super) struct HostStagingDirectory {
    parent: File,
    stage_name: String,
    stage_path: PathBuf,
    active: bool,
}

impl HostStagingDirectory {
    pub(super) fn create(parent: &Path) -> Result<Self> {
        let parent = open_host_directory(parent)?;
        for _ in 0..16 {
            let stage_name = format!(".enclave-cp-{}", Uuid::new_v4());
            let name = cstring(&stage_name)?;
            let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
            if result == 0 {
                let stage_path = PathBuf::from("/proc/self/fd")
                    .join(parent.as_raw_fd().to_string())
                    .join(&stage_name);
                return Ok(Self {
                    parent,
                    stage_name,
                    stage_path,
                    active: true,
                });
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) {
                return Err(error).context("failed to create host transfer staging directory");
            }
        }
        bail!("failed to allocate a unique host transfer staging directory")
    }

    pub(super) fn path(&self) -> &Path {
        &self.stage_path
    }

    pub(super) fn commit(mut self, source_name: &str, target_name: &str) -> Result<()> {
        ensure_host_entry_absent(&self.parent, target_name)?;
        let source = cstring(&format!("{}/{}", self.stage_name, source_name))?;
        let target = cstring(target_name)?;
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                self.parent.as_raw_fd(),
                source.as_ptr(),
                self.parent.as_raw_fd(),
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "failed to commit staged host transfer '{}' as '{}'",
                    source_name, target_name
                )
            });
        }
        self.active = false;
        let _ = fs::remove_dir(&self.stage_path);
        Ok(())
    }
}

impl Drop for HostStagingDirectory {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::remove_dir_all(&self.stage_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::splice_to_pipe;
    use std::fs;
    use std::io::Read;
    use std::os::fd::FromRawFd;

    #[test]
    fn splice_streams_regular_file_into_pipe_or_reports_unsupported() {
        let path = std::env::temp_dir().join(format!(
            "enclave-splice-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, b"splice fixture").expect("write source");
        let source = fs::File::open(&path).expect("open source");
        let mut pipe = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let result = splice_to_pipe(&source, pipe[1], 14).expect("splice result");
        unsafe { libc::close(pipe[1]) };
        if result {
            let mut output = String::new();
            unsafe { fs::File::from_raw_fd(pipe[0]) }
                .read_to_string(&mut output)
                .expect("read pipe");
            assert_eq!(output, "splice fixture");
        } else {
            unsafe { libc::close(pipe[0]) };
        }
        let _ = fs::remove_file(path);
    }
}
