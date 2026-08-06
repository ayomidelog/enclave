use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tar::Archive;
use uuid::Uuid;

use super::path::DestinationPlan;
use crate::workspace::exec::spawn_workspace_command;
use crate::workspace::types::WorkspaceMetadata;

#[derive(Debug)]
pub(super) struct TransferOutput {
    pub(super) logical_bytes: u64,
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

pub(super) fn workspace_path_is_directory(
    workspace: &WorkspaceMetadata,
    path: &str,
    client_stream: Option<&UnixStream>,
) -> Result<bool> {
    let output = run_workspace_utility(workspace, &["test", "-d", path], client_stream)?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    bail!(
        "failed to inspect workspace destination '{}': {}",
        path,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

pub(super) fn validate_workspace_source(
    workspace: &WorkspaceMetadata,
    path: &str,
    client_stream: Option<&UnixStream>,
) -> Result<()> {
    let output = run_workspace_utility(
        workspace,
        &[
            "sh",
            "-c",
            "unsupported=$(find \"$1\" \\( -type p -o -type b -o -type c -o -type s \\) -print -quit) || exit 2; test -z \"$unsupported\"",
            "sh",
            path,
        ],
        client_stream,
    )?;
    if output.status.success() {
        return Ok(());
    }
    if output.status.code() == Some(1) {
        bail!(
            "refusing to copy workspace source '{}' with special files",
            path
        );
    }
    bail!(
        "failed to validate workspace source '{}': {}",
        path,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

pub(super) fn workspace_path_has_symlink(
    workspace: &WorkspaceMetadata,
    path: &str,
    client_stream: Option<&UnixStream>,
) -> Result<()> {
    let mut prefix = PathBuf::new();
    for component in Path::new(path).components() {
        prefix.push(component.as_os_str());
        if prefix == Path::new("/") {
            continue;
        }
        let output = run_workspace_utility(
            workspace,
            &["test", "-L", &prefix.to_string_lossy()],
            client_stream,
        )?;
        if output.status.success() {
            bail!(
                "refusing to copy through symlinked workspace destination component '{}'",
                prefix.display()
            );
        }
    }
    Ok(())
}

pub(super) fn run_host_to_workspace(
    workspace: &WorkspaceMetadata,
    source: &str,
    destination: &DestinationPlan,
    source_name: &str,
    logical_bytes: u64,
    client_stream: Option<&UnixStream>,
) -> Result<TransferOutput> {
    let stage =
        create_workspace_staging_directory(workspace, destination, source_name, client_stream)?;
    let result = (|| {
        let mut host_tar =
            ChildGuard::new(spawn_tar(tar_create_args(source, source_name), true, None)?);
        let stream = host_tar
            .child
            .stdout
            .take()
            .context("host tar stdout unavailable")?;
        let mut workspace_tar = ChildGuard::new(spawn_workspace_command(
            workspace,
            "/home",
            &workspace_tar_command(tar_extract_args_at(&stage)),
            Stdio::from(stream),
            Stdio::null(),
            Stdio::piped(),
        )?);

        let workspace_output = wait_child_output(&mut workspace_tar, client_stream)
            .context("failed to run workspace tar extractor")?;
        let host_output = wait_child_output(&mut host_tar, client_stream)
            .context("failed to wait for host tar")?;
        ensure_transfer_success("host tar", &host_output)?;
        ensure_transfer_success("workspace tar", &workspace_output)?;
        move_workspace_path(
            workspace,
            &format!("{stage}/{source_name}"),
            &destination.final_path(source_name).to_string_lossy(),
            client_stream,
        )?;
        Ok(TransferOutput { logical_bytes })
    })();
    remove_workspace_staging_directory(workspace, &stage);
    result
}

pub(super) fn run_workspace_to_host(
    workspace: &WorkspaceMetadata,
    source: &str,
    destination: &DestinationPlan,
    source_name: &str,
    client_stream: Option<&UnixStream>,
) -> Result<TransferOutput> {
    let stage = HostStagingDirectory::create(&destination.parent)?;
    let mut workspace_tar = ChildGuard::new(spawn_workspace_command(
        workspace,
        "/home",
        &workspace_tar_command(tar_create_args(source, source_name)),
        Stdio::null(),
        Stdio::piped(),
        Stdio::piped(),
    )?);
    let stream = workspace_tar
        .child
        .stdout
        .take()
        .context("workspace tar stdout unavailable")?;
    let logical_bytes = extract_workspace_archive(stream, &stage, source_name)
        .context("failed to validate workspace tar archive")?;
    let workspace_output = wait_child_output(&mut workspace_tar, client_stream)
        .context("failed to wait for workspace tar")?;
    ensure_transfer_success("workspace tar", &workspace_output)?;
    stage.commit(source_name, destination.final_name(source_name))?;
    Ok(TransferOutput { logical_bytes })
}

fn create_workspace_staging_directory(
    workspace: &WorkspaceMetadata,
    destination: &DestinationPlan,
    source_name: &str,
    client_stream: Option<&UnixStream>,
) -> Result<String> {
    let target = destination.final_path(source_name);
    if workspace_path_exists(workspace, &target.to_string_lossy(), client_stream)? {
        bail!(
            "workspace destination '{}' already exists",
            target.display()
        );
    }
    let stage = destination
        .parent
        .join(format!(".enclave-cp-{}", Uuid::new_v4()))
        .to_string_lossy()
        .to_string();
    let output = run_workspace_utility(workspace, &["mkdir", "--", &stage], client_stream)?;
    ensure_transfer_success("workspace staging directory", &output)?;
    Ok(stage)
}

fn remove_workspace_staging_directory(workspace: &WorkspaceMetadata, stage: &str) {
    let _ = run_workspace_utility(workspace, &["rm", "-rf", "--", stage], None);
}

fn workspace_path_exists(
    workspace: &WorkspaceMetadata,
    path: &str,
    client_stream: Option<&UnixStream>,
) -> Result<bool> {
    let output = run_workspace_utility(workspace, &["test", "-e", path], client_stream)?;
    if output.status.success() {
        return Ok(true);
    }
    let symlink = run_workspace_utility(workspace, &["test", "-L", path], client_stream)?;
    if symlink.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) && symlink.status.code() == Some(1) {
        return Ok(false);
    }
    bail!("failed to inspect workspace destination '{}'", path)
}

fn move_workspace_path(
    workspace: &WorkspaceMetadata,
    source: &str,
    target: &str,
    client_stream: Option<&UnixStream>,
) -> Result<()> {
    let output = run_workspace_utility(workspace, &["mv", "--", source, target], client_stream)?;
    ensure_transfer_success("workspace staged move", &output)
}

pub(super) fn extract_workspace_archive<R: Read>(
    stream: R,
    stage: &HostStagingDirectory,
    source_name: &str,
) -> Result<u64> {
    let mut archive = Archive::new(stream);
    let mut logical_bytes = 0u64;
    let mut entries = HashSet::new();
    for entry in archive
        .entries()
        .context("failed to read workspace tar entries")?
    {
        let mut entry = entry.context("failed to read workspace tar entry")?;
        let path = entry
            .path()
            .context("workspace tar entry has an invalid path")?
            .into_owned();
        validate_archive_path(&path, source_name)?;
        validate_archive_entry_type(entry.header().entry_type())?;
        if !entries.insert(path.clone()) {
            bail!(
                "workspace tar archive contains duplicate entry '{}'",
                path.display()
            );
        }
        ensure_no_symlink_ancestors(stage.path(), &path)?;
        if entry.header().entry_type().is_file() {
            logical_bytes = logical_bytes.saturating_add(entry.header().size()?);
        }
        entry.unpack_in(stage.path()).with_context(|| {
            format!("failed to unpack workspace tar entry '{}'", path.display())
        })?;
    }
    let root = stage.path().join(source_name);
    if fs::symlink_metadata(&root).is_err() {
        bail!("workspace tar archive did not contain expected entry '{source_name}'");
    }
    Ok(logical_bytes)
}

pub(super) fn validate_archive_path(path: &Path, source_name: &str) -> Result<()> {
    let mut components = path.components();
    let Some(Component::Normal(root)) = components.next() else {
        bail!(
            "workspace tar archive contains unsafe entry '{}'",
            path.display()
        );
    };
    if root != source_name {
        bail!(
            "workspace tar archive contains unexpected entry '{}'; expected '{}...'",
            path.display(),
            source_name
        );
    }
    for component in components {
        if !matches!(component, Component::Normal(_)) {
            bail!(
                "workspace tar archive contains unsafe entry '{}'",
                path.display()
            );
        }
    }
    Ok(())
}

pub(super) fn validate_archive_entry_type(entry_type: tar::EntryType) -> Result<()> {
    if entry_type.is_file() || entry_type.is_dir() || entry_type.is_symlink() {
        return Ok(());
    }
    bail!("workspace tar archive contains unsupported entry type")
}

fn ensure_no_symlink_ancestors(root: &Path, path: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in path.components() {
        let Component::Normal(component) = component else {
            bail!(
                "workspace tar archive contains unsafe entry '{}'",
                path.display()
            );
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "workspace tar archive writes through symlinked entry '{}'",
                    current.display()
                )
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to inspect staged entry {}", current.display())
                })
            }
        }
    }
    Ok(())
}

pub(super) fn open_host_directory(path: &Path) -> Result<File> {
    if !path.is_absolute() {
        bail!(
            "host destination parent must be absolute: {}",
            path.display()
        );
    }
    let mut current = File::open("/").context("failed to open host root directory")?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => {
                let name = cstring_os(name)?;
                let descriptor = unsafe {
                    libc::openat(
                        current.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if descriptor < 0 {
                    return Err(std::io::Error::last_os_error()).with_context(|| {
                        format!(
                            "failed to securely open host destination directory {}",
                            path.display()
                        )
                    });
                }
                current = unsafe { File::from_raw_fd(descriptor) };
            }
            Component::ParentDir | Component::Prefix(_) => {
                bail!(
                    "host destination parent contains unsafe component: {}",
                    path.display()
                )
            }
        }
    }
    Ok(current)
}

fn ensure_host_entry_absent(parent: &File, name: &str) -> Result<()> {
    let name = cstring(name)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        bail!(
            "host destination '{}' already exists",
            name.to_string_lossy()
        );
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        return Ok(());
    }
    Err(error).context("failed to inspect host destination entry")
}

fn cstring(value: &str) -> Result<CString> {
    CString::new(value).map_err(|_| anyhow!("path contains an interior NUL byte"))
}

fn cstring_os(value: &std::ffi::OsStr) -> Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| anyhow!("path contains an interior NUL byte"))
}

fn spawn_tar(args: Vec<String>, creating: bool, input: Option<Stdio>) -> Result<Child> {
    let mut command = Command::new("tar");
    let extracting = !creating;
    command
        .args(args)
        .stdin(if extracting {
            input.unwrap_or_else(Stdio::piped)
        } else {
            Stdio::null()
        })
        .stdout(if creating {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start tar")
}

fn tar_create_args(source: &str, source_name: &str) -> Vec<String> {
    let parent = Path::new(source)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("/"));
    vec![
        "-C".into(),
        parent.to_string_lossy().into_owned(),
        "-cf".into(),
        "-".into(),
        "--".into(),
        source_name.into(),
    ]
}

fn tar_extract_args_at(destination: &str) -> Vec<String> {
    vec![
        "-C".into(),
        destination.into(),
        "-xpf".into(),
        "-".into(),
        "-o".into(),
        "--".into(),
    ]
}

pub(super) fn workspace_tar_command(args: Vec<String>) -> Vec<String> {
    let mut command = vec!["tar".to_string()];
    command.extend(args);
    command
}

fn ensure_transfer_success(label: &str, output: &Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "{} failed (exit {}): {}",
        label,
        output
            .status
            .code()
            .map_or_else(|| "signal".to_string(), |code| code.to_string()),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn run_workspace_utility(
    workspace: &WorkspaceMetadata,
    command: &[&str],
    client_stream: Option<&UnixStream>,
) -> Result<Output> {
    let primary = spawn_workspace_command(
        workspace,
        "/home",
        &command
            .iter()
            .map(|item| (*item).to_string())
            .collect::<Vec<_>>(),
        Stdio::null(),
        Stdio::null(),
        Stdio::piped(),
    )?;
    let mut primary = ChildGuard::new(primary);
    let output = wait_child_output(&mut primary, client_stream)?;
    if output.status.code() != Some(127) {
        return Ok(output);
    }

    let mut fallback_command = vec!["/bin/busybox".to_string()];
    fallback_command.extend(command.iter().map(|item| (*item).to_string()));
    let fallback = spawn_workspace_command(
        workspace,
        "/home",
        &fallback_command,
        Stdio::null(),
        Stdio::null(),
        Stdio::piped(),
    )?;
    let mut fallback = ChildGuard::new(fallback);
    wait_child_output(&mut fallback, client_stream)
}

pub(super) fn wait_child_output(
    child: &mut ChildGuard,
    client_stream: Option<&UnixStream>,
) -> Result<Output> {
    let stderr_reader = child.child.stderr.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut stderr = Vec::new();
            pipe.read_to_end(&mut stderr).map(|_| stderr)
        })
    });
    let status = loop {
        match child
            .child
            .try_wait()
            .context("failed to poll child process")?
        {
            Some(status) => break status,
            None => {
                if client_stream.is_some_and(|stream| !client_is_connected(stream)) {
                    let _ = child.child.kill();
                    let _ = child.child.wait();
                    if let Some(reader) = stderr_reader {
                        let _ = reader.join();
                    }
                    bail!("workspace cp client disconnected; transfer cancelled")
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    };
    let stderr = match stderr_reader {
        Some(reader) => reader
            .join()
            .map_err(|_| anyhow!("child stderr reader panicked"))??,
        None => Vec::new(),
    };
    child.disarm();
    Ok(Output {
        status,
        stdout: Vec::new(),
        stderr,
    })
}

fn client_is_connected(stream: &UnixStream) -> bool {
    let mut descriptor = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if result < 0 {
        return true;
    }
    descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) == 0
}
